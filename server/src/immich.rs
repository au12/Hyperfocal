//! Send to Immich: photos picked here go to an Immich server, into an album if asked.
//!
//! A photo Immich already has is used as it is, never uploaded twice:
//! - found by its path, when Immich reads this library as an external library
//!   (`HYPERFOCAL_IMMICH_LIBRARY_PATH`: where Immich sees the photo folder);
//! - found by its content (SHA-1), when the same file was uploaded before;
//! - remembered here, when Hyperfocal sent this version of the photo before.
//!
//! Anything else is uploaded. RAW files go as a full-size JPEG by default (the camera's
//! own JPEG of a RAW+JPEG pair, else the JPEG embedded in the RAW, carrying the photo's
//! metadata: capture time, camera, GPS, rating, keywords), so they look the same in
//! Immich as here and stay small; or as the original file. Other photos go as they are.
//! The XMP sidecar goes along, so ratings, titles and keywords come with them.
//!
//! Nothing in Immich is ever changed or deleted besides adding to albums, and sending
//! changes nothing in the library.

use crate::api::{ApiError, ApiResult, St, bad};
use crate::ops::Job;
use crate::state::AppState;
use anyhow::{Context, Result, anyhow, bail};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(
        r#"
        -- Photos sent to Immich: which asset they are there, for which version of the
        -- file (sig), and how they got there (library, checksum, original, jpeg).
        CREATE TABLE IF NOT EXISTS immich_assets(
            photo_id INTEGER PRIMARY KEY REFERENCES photos(id) ON DELETE CASCADE,
            sig TEXT NOT NULL,
            asset_id TEXT NOT NULL,
            how TEXT NOT NULL,
            at INTEGER NOT NULL
        );
        "#,
    )?;
    Ok(())
}

// ---------------------------------------------------------------- client

#[derive(Clone)]
pub struct Client {
    base: String,
    key: String,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub count: i64,
    pub thumb: Option<String>,
}

/// What became of an upload: the asset, and whether Immich had it already.
#[derive(Debug, Clone, PartialEq)]
pub struct Uploaded {
    pub id: String,
    pub duplicate: bool,
}

/// A file going to Immich.
pub struct Upload<'a> {
    pub data: &'a [u8],
    pub filename: &'a str,
    pub sidecar: Option<&'a [u8]>,
    /// RFC 3339.
    pub created: &'a str,
    pub modified: &'a str,
    pub device_asset_id: &'a str,
}

fn sha1_b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(Sha1::digest(data))
}

impl Client {
    pub fn new(base: &str, key: &str) -> Client {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(8)))
            // A 60 MB RAW over a slow link.
            .timeout_global(Some(Duration::from_secs(600)))
            .http_status_as_error(false)
            .build()
            .into();
        let base = base.trim_end_matches('/').trim_end_matches("/api").to_string();
        Client { base, key: key.to_string(), agent }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/{}", self.base, path.trim_start_matches('/'))
    }

    /// Reads an answer: JSON on success, Immich's message otherwise.
    fn answer(&self, what: &str, r: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<(u16, Value)> {
        let mut r = r.map_err(|e| anyhow!("Immich ({}) cannot be reached: {e}", self.base))?;
        let status = r.status().as_u16();
        let body = r.body_mut().with_config().limit(64 << 20).read_to_string().unwrap_or_default();
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let msg = v["message"]
                .as_str()
                .map(str::to_string)
                .or_else(|| v["message"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")))
                .unwrap_or_else(|| body.chars().take(200).collect());
            if status == 401 || status == 403 {
                bail!("Immich refused the API key for {what} ({status}): {msg}. The key needs the asset.read, asset.upload, album.read, album.create and albumAsset.create permissions.");
            }
            bail!("Immich answered {status} to {what}: {msg}");
        }
        Ok((status, v))
    }

    fn get(&self, path: &str) -> Result<(u16, Value)> {
        let r = self.agent.get(&self.url(path)).header("x-api-key", &self.key).header("accept", "application/json").call();
        self.answer(path, r)
    }

    fn send(&self, method: &str, path: &str, body: &Value) -> Result<(u16, Value)> {
        let body = body.to_string();
        let url = self.url(path);
        let r = match method {
            "PUT" => self.agent.put(&url).header("x-api-key", &self.key).content_type("application/json").send(&body),
            _ => self.agent.post(&url).header("x-api-key", &self.key).content_type("application/json").send(&body),
        };
        self.answer(path, r)
    }

    /// Albums the key's user can add to.
    pub fn albums(&self) -> Result<Vec<Album>> {
        let (_, v) = self.get("albums")?;
        let mut out: Vec<Album> = v
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        Some(Album {
                            id: x["id"].as_str()?.to_string(),
                            name: x["albumName"].as_str().unwrap_or("").to_string(),
                            count: x["assetCount"].as_i64().unwrap_or(0),
                            thumb: x["albumThumbnailAssetId"].as_str().map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|a| a.name.to_lowercase());
        Ok(out)
    }

    /// A new album, with these assets in it.
    pub fn create_album(&self, name: &str, assets: &[String]) -> Result<String> {
        let (_, v) = self.send("POST", "albums", &json!({ "albumName": name, "assetIds": assets }))?;
        v["id"].as_str().map(str::to_string).context("Immich did not say which album it created")
    }

    /// Adds assets to an album: returns (added, already there).
    pub fn add_to_album(&self, album: &str, assets: &[String]) -> Result<(usize, usize)> {
        let (mut added, mut already) = (0, 0);
        for chunk in assets.chunks(500) {
            let (_, v) = self.send("PUT", &format!("albums/{album}/assets"), &json!({ "ids": chunk }))?;
            for r in v.as_array().into_iter().flatten() {
                if r["success"].as_bool() == Some(true) {
                    added += 1;
                } else if r["error"].as_str() == Some("duplicate") {
                    already += 1;
                } else {
                    bail!("Immich did not add an asset to the album: {}", r["error"].as_str().unwrap_or("unknown error"));
                }
            }
        }
        Ok((added, already))
    }

    fn search(&self, body: Value) -> Result<Vec<Value>> {
        let (_, v) = self.send("POST", "search/metadata", &body)?;
        Ok(v["assets"]["items"].as_array().cloned().unwrap_or_default())
    }

    /// The asset at this path (as Immich sees it), not in the trash.
    pub fn find_by_path(&self, path: &str) -> Result<Option<String>> {
        let items = self.search(json!({ "originalPath": path, "withDeleted": false, "size": 100 }))?;
        Ok(items
            .iter()
            .find(|a| a["originalPath"].as_str() == Some(path) && a["isTrashed"].as_bool() != Some(true))
            .and_then(|a| a["id"].as_str().map(str::to_string)))
    }

    /// An asset with this content (SHA-1, base64), not in the trash.
    pub fn find_by_checksum(&self, checksum: &str) -> Result<Option<String>> {
        let items = self.search(json!({ "checksum": checksum, "withDeleted": false, "size": 10 }))?;
        Ok(items
            .iter()
            .find(|a| a["checksum"].as_str().is_none_or(|c| c == checksum) && a["isTrashed"].as_bool() != Some(true))
            .and_then(|a| a["id"].as_str().map(str::to_string)))
    }

    /// Whether an asset is still there (and not in the trash).
    pub fn exists(&self, id: &str) -> Result<bool> {
        let r = self.agent.get(&self.url(&format!("assets/{id}"))).header("x-api-key", &self.key).call();
        match self.answer("an asset", r) {
            Ok((_, v)) => Ok(v["isTrashed"].as_bool() != Some(true)),
            Err(e) if format!("{e}").contains("answered 400") || format!("{e}").contains("answered 404") => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn upload(&self, u: &Upload) -> Result<Uploaded> {
        let boundary = format!("hyperfocal{:016x}", rand::random::<u64>());
        let mut body: Vec<u8> = Vec::with_capacity(u.data.len() + u.sidecar.map_or(0, <[u8]>::len) + 2048);
        let field = |b: &mut Vec<u8>, name: &str, value: &str| {
            b.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
        };
        let file = |b: &mut Vec<u8>, name: &str, filename: &str, mime: &str, data: &[u8]| {
            let safe: String = filename.chars().map(|c| if c == '"' || c == '\r' || c == '\n' { '_' } else { c }).collect();
            b.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{safe}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
            b.extend_from_slice(data);
            b.extend_from_slice(b"\r\n");
        };
        field(&mut body, "deviceAssetId", u.device_asset_id);
        field(&mut body, "deviceId", "Hyperfocal");
        field(&mut body, "fileCreatedAt", u.created);
        field(&mut body, "fileModifiedAt", u.modified);
        field(&mut body, "filename", u.filename);
        field(&mut body, "isFavorite", "false");
        file(&mut body, "assetData", u.filename, "application/octet-stream", u.data);
        if let Some(x) = u.sidecar {
            file(&mut body, "sidecarData", &format!("{}.xmp", u.filename), "application/xml", x);
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        let r = self
            .agent
            .post(&self.url("assets"))
            .header("x-api-key", &self.key)
            // Immich answers "duplicate" without storing a second copy.
            .header("x-immich-checksum", &sha1_b64(u.data))
            .content_type(&format!("multipart/form-data; boundary={boundary}"))
            .send(&body[..]);
        let (status, v) = self.answer("an upload", r)?;
        let id = v["id"].as_str().context("Immich did not say which asset it made")?.to_string();
        Ok(Uploaded { id, duplicate: status == 200 || v["status"].as_str() == Some("duplicate") })
    }
}

// ---------------------------------------------------------------- what to send

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// RAW files as a full-size JPEG, everything else as it is.
    #[default]
    Jpeg,
    /// Every file as it is.
    Original,
}

/// Where Immich sees a file of the library, when it reads the library as an external
/// library: its library path joined with the file's path in the library.
pub fn immich_path(root: &str, folder: &str, name: &str) -> String {
    let root = root.trim_end_matches('/');
    if folder.is_empty() { format!("{root}/{name}") } else { format!("{root}/{folder}/{name}") }
}

/// Seconds on the camera clock (or file time) as RFC 3339.
fn rfc3339(secs: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".into())
}

/// The camera's own JPEG of a RAW+JPEG pair (`companion` lists the files folded in).
fn companion_jpeg(companion: Option<&str>) -> Option<String> {
    companion?
        .split(',')
        .find(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".jpg") || l.ends_with(".jpeg")
        })
        .map(str::to_string)
}

fn remember(state: &AppState, photo: i64, sig: &str, asset: &str, how: &str) -> Result<()> {
    state.db.get()?.execute(
        "INSERT INTO immich_assets(photo_id, sig, asset_id, how, at) VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(photo_id) DO UPDATE SET sig = excluded.sig, asset_id = excluded.asset_id, how = excluded.how, at = excluded.at",
        params![photo, sig, asset, how, crate::db::now_ms()],
    )?;
    Ok(())
}

fn remembered(state: &AppState, photo: i64) -> Result<Option<(String, String)>> {
    Ok(state.db.get()?.query_row("SELECT sig, asset_id FROM immich_assets WHERE photo_id = ?1", [photo], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
}

/// How one photo got to Immich.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    Known,
    Library,
    Checksum,
    Uploaded,
    Duplicate,
}

/// A full-size JPEG of a RAW file with the photo's metadata copied in: from the RAW
/// (camera, lens, exposure, capture time, GPS) and its sidecar (rating, label,
/// keywords, title, places), with the orientation of the photo.
async fn jpeg_with_metadata(state: &Arc<AppState>, exif: &mut crate::meta::exiftool::ExifTool, loc: &crate::repo::Located, raw: &Path, sidecar: Option<&Path>) -> Result<Vec<u8>> {
    let bytes = state.images.full_jpeg(state, loc).await?;
    let dir = state.cfg.cache_dir.join("immich");
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{}-{:x}.jpg", loc.rec.id, rand::random::<u32>()));
    std::fs::write(&tmp, &bytes)?;
    let mut args: Vec<String> = vec!["-overwrite_original".into(), "-m".into(), "-charset".into(), "filename=utf8".into()];
    args.extend(["-TagsFromFile".into(), raw.to_string_lossy().into_owned(), "-EXIF:All".into(), "-GPS:All".into(), "-IPTC:All".into(), "-XMP:All".into()]);
    if let Some(sc) = sidecar {
        args.extend(["-TagsFromFile".into(), sc.to_string_lossy().into_owned(), "-XMP:All".into()]);
    }
    // The pixels are as the sensor recorded them; the tag turns them upright.
    args.push(format!("-EXIF:Orientation#={}", loc.rec.orientation.clamp(1, 8)));
    args.push("-EXIF:ThumbnailImage=".into());
    args.push(tmp.to_string_lossy().into_owned());
    let out = exif.exec(&args).await;
    let data = std::fs::read(&tmp);
    let _ = std::fs::remove_file(&tmp);
    let out = out?;
    if out.changed() == 0 {
        tracing::warn!("copying metadata into the JPEG of {}: {}", loc.rec.name, out.error().unwrap_or_default());
    }
    Ok(data?)
}

/// Gets one photo into Immich: returns the asset and how.
async fn send_one(state: &Arc<AppState>, client: &Client, exif: &mut Option<crate::meta::exiftool::ExifTool>, id: i64, mode: Mode) -> Result<(String, How)> {
    let st = state.clone();
    let loc = tokio::task::spawn_blocking(move || -> Result<Option<crate::repo::Located>> { crate::repo::load(&*st.db.get()?, id) })
        .await??
        .ok_or_else(|| anyhow!("the photo is no longer in the library"))?;
    let rec = &loc.rec;
    let sig = crate::images::sig_str(rec);
    let dir = crate::scan::abs_dir(state, &loc.folder_path);
    let raw = rec.kind == crate::model::Kind::Raw as i32;
    let pair_jpeg = if raw { companion_jpeg(rec.companion.as_deref()) } else { None };

    // Sent before, and the file has not changed since.
    if let Some((s, asset)) = remembered(state, id)? {
        if s == sig {
            let (c, a) = (client.clone(), asset.clone());
            if tokio::task::spawn_blocking(move || c.exists(&a)).await?? {
                return Ok((asset, How::Known));
            }
        }
    }
    // In Immich's external library: by path (for JPEG mode, the camera JPEG first).
    if let Some(root) = state.cfg.immich_library_path.as_deref() {
        let mut names = vec![rec.name.clone()];
        if let Some(j) = &pair_jpeg {
            if mode == Mode::Jpeg { names.insert(0, j.clone()) } else { names.push(j.clone()) }
        }
        for n in names {
            let (c, p) = (client.clone(), immich_path(root, &loc.folder_path, &n));
            if let Some(asset) = tokio::task::spawn_blocking(move || c.find_by_path(&p)).await?? {
                remember(state, id, &sig, &asset, "library")?;
                return Ok((asset, How::Library));
            }
        }
    }
    // The same file, uploaded before (by Hyperfocal, the CLI or a phone).
    let path = dir.join(&rec.name);
    let original = tokio::fs::read(&path).await.with_context(|| format!("reading {}", rec.name))?;
    let (c, sum) = (client.clone(), sha1_b64(&original));
    if let Some(asset) = tokio::task::spawn_blocking(move || c.find_by_checksum(&sum)).await?? {
        remember(state, id, &sig, &asset, "checksum")?;
        return Ok((asset, How::Checksum));
    }

    let sidecar_path = rec.sidecar.as_ref().map(|s| dir.join(s)).filter(|p| p.exists());
    let sidecar = match &sidecar_path {
        Some(p) => Some(tokio::fs::read(p).await?),
        None => None,
    };
    let created = rfc3339(rec.taken.unwrap_or(rec.mtime / 1000));
    let modified = rfc3339(rec.mtime / 1000);
    let device_asset_id = format!("hyperfocal-{id}-{sig}");
    let (data, filename, how): (Vec<u8>, String, &str) = if raw && mode == Mode::Jpeg {
        match &pair_jpeg {
            // The camera's own JPEG: its processing and its metadata, as shot.
            Some(j) => (tokio::fs::read(dir.join(j)).await.with_context(|| format!("reading {j}"))?, j.clone(), "jpeg"),
            None => {
                if exif.is_none() {
                    *exif = Some(crate::meta::exiftool::ExifTool::new(&state.cfg.exiftool, &state.cfg.config_dir)?);
                }
                let e = exif.as_mut().unwrap();
                let jpeg = jpeg_with_metadata(state, e, &loc, &path, sidecar_path.as_deref()).await?;
                (jpeg, format!("{}.jpg", rec.stem), "jpeg")
            }
        }
    } else {
        (original, rec.name.clone(), "original")
    };
    let c = client.clone();
    let up = tokio::task::spawn_blocking(move || {
        c.upload(&Upload {
            data: &data,
            filename: &filename,
            sidecar: sidecar.as_deref(),
            created: &created,
            modified: &modified,
            device_asset_id: &device_asset_id,
        })
    })
    .await??;
    remember(state, id, &sig, &up.id, how)?;
    Ok((up.id, if up.duplicate { How::Duplicate } else { How::Uploaded }))
}

/// Where the photos go: an album of Immich's, a new one, or none.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SendBody {
    pub ids: Vec<i64>,
    /// An existing album (its id).
    pub album: Option<String>,
    /// A new album of this name.
    pub album_name: Option<String>,
    #[serde(default)]
    pub mode: Mode,
}

fn plural(n: usize) -> String {
    if n == 1 { "1 photo".into() } else { format!("{n} photos") }
}

/// Sends photos in the background: progress in the header, a summary at the end.
pub fn spawn_send(state: &Arc<AppState>, client: Client, b: SendBody) -> crate::ops::JobStatus {
    let batch = crate::ops::new_batch();
    let mut job = Job::start(state, "Sending to Immich".into(), b.ids.len(), batch);
    job.status.undo = false;
    let status = job.status.clone();
    let st = state.clone();
    tokio::spawn(async move {
        let public = st.cfg.immich_public_url.clone().or_else(|| st.cfg.immich_url.clone()).unwrap_or_default();
        let mut exif: Option<crate::meta::exiftool::ExifTool> = None;
        let mut assets: Vec<String> = Vec::new();
        let (mut uploaded, mut found) = (0usize, 0usize);
        for &id in &b.ids {
            match send_one(&st, &client, &mut exif, id, b.mode).await {
                Ok((asset, how)) => {
                    if how == How::Uploaded {
                        uploaded += 1;
                    } else {
                        found += 1;
                    }
                    if !assets.contains(&asset) {
                        assets.push(asset);
                    }
                    job.tick(None);
                }
                Err(e) => {
                    tracing::warn!("sending photo {id} to Immich: {e:#}");
                    job.tick(Some((id, format!("{e:#}"))));
                }
            }
        }
        // Then the album.
        let mut album_note = String::new();
        if !assets.is_empty() {
            let c = client.clone();
            let (name, album) = (b.album_name.clone(), b.album.clone());
            let list = assets.clone();
            let r = tokio::task::spawn_blocking(move || -> Result<Option<(String, String)>> {
                if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
                    let id = c.create_album(n.trim(), &list)?;
                    Ok(Some((id, n.trim().to_string())))
                } else if let Some(a) = album {
                    c.add_to_album(&a, &list)?;
                    let name = c.albums().ok().and_then(|all| all.into_iter().find(|x| x.id == a).map(|x| x.name)).unwrap_or_else(|| "the album".into());
                    Ok(Some((a, name)))
                } else {
                    Ok(None)
                }
            })
            .await
            .map_err(|e| anyhow!("{e}"))
            .and_then(|r| r);
            match r {
                Ok(Some((aid, name))) => {
                    album_note = format!(" · in “{name}”");
                    job.status.link = Some(format!("{}/albums/{aid}", public.trim_end_matches('/')));
                }
                Ok(None) => {}
                Err(e) => {
                    job.status.failed += 1;
                    job.status.errors.push((0, format!("{e:#}")));
                    album_note = " · not added to the album".into();
                }
            }
        }
        let sent = uploaded + found;
        let detail = match (uploaded, found) {
            (0, 0) => String::new(),
            (u, 0) => format!(" ({u} uploaded)"),
            (0, _) => " (Immich had them already)".into(),
            (u, f) => format!(" ({u} uploaded, {f} already there)"),
        };
        job.status.summary = Some(format!("Sent {} to Immich{detail}{album_note}", plural(sent)));
        if job.status.link.is_none() && sent > 0 && !public.is_empty() {
            job.status.link = Some(public.trim_end_matches('/').to_string());
        }
        job.finish();
    });
    status
}

// ---------------------------------------------------------------- HTTP

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/immich/albums", get(albums)).route("/immich/send", post(send))
}

pub fn client(state: &AppState) -> Option<Client> {
    Some(Client::new(state.cfg.immich_url.as_deref()?, state.cfg.immich_api_key.as_deref()?))
}

fn not_configured() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "Immich is not set up: set HYPERFOCAL_IMMICH_URL and HYPERFOCAL_IMMICH_API_KEY".into())
}

/// For `/api/status`: whether sending is set up, and where Immich is for links.
pub fn status(state: &AppState) -> Value {
    match (&state.cfg.immich_url, &state.cfg.immich_api_key) {
        (Some(url), Some(_)) => json!({
            "url": state.cfg.immich_public_url.as_deref().unwrap_or(url),
            "library": state.cfg.immich_library_path.is_some(),
        }),
        _ => Value::Null,
    }
}

async fn albums(State(state): St) -> ApiResult<Json<Value>> {
    let c = client(&state).ok_or_else(not_configured)?;
    let list = tokio::task::spawn_blocking(move || c.albums())
        .await
        .map_err(|e| anyhow!("{e}"))?
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    Ok(Json(json!(list.into_iter().map(|a| json!({ "id": a.id, "name": a.name, "count": a.count, "thumb": a.thumb })).collect::<Vec<_>>())))
}

async fn send(State(state): St, Json(b): Json<SendBody>) -> ApiResult<Json<Value>> {
    let c = client(&state).ok_or_else(not_configured)?;
    if b.ids.is_empty() || b.ids.len() > 20_000 {
        return Err(bad("between 1 and 20000 photos at a time"));
    }
    if b.album_name.as_deref().is_some_and(|n| n.trim().chars().count() > 200) {
        return Err(bad("an album name of at most 200 characters"));
    }
    Ok(Json(json!({ "job": spawn_send(&state, c, b) })))
}

/// The file an upload of a photo would carry, for tests and the docs.
#[cfg(test)]
fn upload_name(rec: &crate::model::PhotoRec, mode: Mode) -> String {
    let raw = rec.kind == crate::model::Kind::Raw as i32;
    match (raw, mode, companion_jpeg(rec.companion.as_deref())) {
        (true, Mode::Jpeg, Some(j)) => j,
        (true, Mode::Jpeg, None) => format!("{}.jpg", rec.stem),
        _ => rec.name.clone(),
    }
}

#[cfg(test)]
pub(crate) mod mock {
    //! A pretend Immich server: albums, search by path or checksum, uploads.
    use axum::body::Bytes;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post, put};
    use axum::{Json, Router};
    use parking_lot::Mutex;
    use serde_json::{Value, json};
    use std::sync::Arc;

    #[derive(Default)]
    pub struct Immich {
        /// (id, originalPath, checksum, filename, has sidecar)
        pub assets: Vec<(String, String, String, String, bool)>,
        pub albums: Vec<(String, String, Vec<String>)>,
        pub uploads: usize,
    }

    pub type Shared = Arc<Mutex<Immich>>;

    fn authed(h: &HeaderMap) -> bool {
        h.get("x-api-key").and_then(|v| v.to_str().ok()) == Some("secret")
    }

    pub async fn serve(state: Shared) -> String {
        let app = Router::new()
            .route("/api/albums", get(albums).post(create))
            .route("/api/albums/{id}/assets", put(add))
            .route("/api/search/metadata", post(search))
            .route("/api/assets", post(upload))
            .route("/api/assets/{id}", get(asset))
            .with_state(state);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn albums(State(s): State<Shared>, h: HeaderMap) -> (StatusCode, Json<Value>) {
        if !authed(&h) {
            return (StatusCode::UNAUTHORIZED, Json(json!({"message": "Invalid API key"})));
        }
        let s = s.lock();
        (StatusCode::OK, Json(json!(s.albums.iter().map(|(id, n, a)| json!({"id": id, "albumName": n, "assetCount": a.len()})).collect::<Vec<_>>())))
    }

    async fn create(State(s): State<Shared>, Json(b): Json<Value>) -> (StatusCode, Json<Value>) {
        let mut s = s.lock();
        let id = format!("album-{}", s.albums.len() + 1);
        let ids = b["assetIds"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        s.albums.push((id.clone(), b["albumName"].as_str().unwrap_or("").to_string(), ids));
        (StatusCode::CREATED, Json(json!({"id": id})))
    }

    async fn add(State(s): State<Shared>, Path(id): Path<String>, Json(b): Json<Value>) -> (StatusCode, Json<Value>) {
        let mut s = s.lock();
        let Some(album) = s.albums.iter_mut().find(|a| a.0 == id) else { return (StatusCode::BAD_REQUEST, Json(json!({"message": "Not found or no album.read access"}))) };
        let mut out = Vec::new();
        for x in b["ids"].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
            if album.2.iter().any(|a| a == x) {
                out.push(json!({"id": x, "success": false, "error": "duplicate"}));
            } else {
                album.2.push(x.to_string());
                out.push(json!({"id": x, "success": true}));
            }
        }
        (StatusCode::OK, Json(json!(out)))
    }

    async fn search(State(s): State<Shared>, Json(b): Json<Value>) -> Json<Value> {
        let s = s.lock();
        let items: Vec<Value> = s
            .assets
            .iter()
            .filter(|a| b["originalPath"].as_str().is_none_or(|p| a.1.to_lowercase().contains(&p.to_lowercase())))
            .filter(|a| b["checksum"].as_str().is_none_or(|c| a.2 == c))
            .map(|a| json!({"id": a.0, "originalPath": a.1, "checksum": a.2, "originalFileName": a.3, "isTrashed": false}))
            .collect();
        Json(json!({"assets": {"total": items.len(), "count": items.len(), "items": items, "nextPage": null}}))
    }

    async fn asset(State(s): State<Shared>, Path(id): Path<String>) -> (StatusCode, Json<Value>) {
        let s = s.lock();
        match s.assets.iter().find(|a| a.0 == id) {
            Some(a) => (StatusCode::OK, Json(json!({"id": a.0, "isTrashed": false}))),
            None => (StatusCode::BAD_REQUEST, Json(json!({"message": "Not found or no asset.read access"}))),
        }
    }

    /// A multipart field's value, or a file's name and bytes.
    fn part<'a>(body: &'a [u8], name: &str) -> Option<(Option<String>, &'a [u8])> {
        let key = format!("name=\"{name}\"");
        let at = body.windows(key.len()).position(|w| w == key.as_bytes())?;
        let head_end = at + body[at..].windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&body[at..head_end]).to_string();
        let filename = head.split("filename=\"").nth(1).and_then(|r| r.split('"').next()).map(str::to_string);
        let start = head_end + 4;
        let end = start + body[start..].windows(4).position(|w| w == b"\r\n--")?;
        Some((filename, &body[start..end]))
    }

    async fn upload(State(s): State<Shared>, h: HeaderMap, body: Bytes) -> (StatusCode, Json<Value>) {
        let ct = h.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
        if !ct.starts_with("multipart/form-data; boundary=") {
            return (StatusCode::BAD_REQUEST, Json(json!({"message": "not multipart"})));
        }
        let Some((Some(filename), data)) = part(&body, "assetData") else { return (StatusCode::BAD_REQUEST, Json(json!({"message": "no file"}))) };
        for f in ["deviceAssetId", "deviceId", "fileCreatedAt", "fileModifiedAt"] {
            if part(&body, f).is_none() {
                return (StatusCode::BAD_REQUEST, Json(json!({"message": format!("{f} is required")})));
            }
        }
        let sum = super::sha1_b64(data);
        let mut s = s.lock();
        if let Some(a) = s.assets.iter().find(|a| a.2 == sum) {
            return (StatusCode::OK, Json(json!({"id": a.0, "status": "duplicate"})));
        }
        s.uploads += 1;
        let id = format!("asset-{}", s.assets.len() + 1);
        let sidecar = part(&body, "sidecarData").is_some();
        s.assets.push((id.clone(), format!("/upload/{filename}"), sum, filename, sidecar));
        (StatusCode::CREATED, Json(json!({"id": id, "status": "created"})))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_as_immich_sees_them() {
        assert_eq!(immich_path("/mnt/photos/", "2019/Trip", "IMG_1.CR3"), "/mnt/photos/2019/Trip/IMG_1.CR3");
        assert_eq!(immich_path("/mnt/photos", "", "a.jpg"), "/mnt/photos/a.jpg");
        assert_eq!(companion_jpeg(Some("IMG_1.HIF,IMG_1.JPG")), Some("IMG_1.JPG".into()));
        assert_eq!(companion_jpeg(Some("IMG_1.HIF")), None);
        let rec = crate::model::PhotoRec { name: "IMG_1.CR3".into(), stem: "IMG_1".into(), kind: crate::model::Kind::Raw as i32, ..Default::default() };
        assert_eq!(upload_name(&rec, Mode::Jpeg), "IMG_1.jpg");
        assert_eq!(upload_name(&rec, Mode::Original), "IMG_1.CR3");
        let pair = crate::model::PhotoRec { companion: Some("IMG_1.JPG".into()), ..rec.clone() };
        assert_eq!(upload_name(&pair, Mode::Jpeg), "IMG_1.JPG");
        assert_eq!(rfc3339(1_560_000_000), "2019-06-08T13:20:00Z");
    }

    #[tokio::test]
    async fn sends_finds_and_never_uploads_twice() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("photos");
        std::fs::create_dir_all(lib.join("2019/Trip")).unwrap();
        // a.jpg: Immich reads the library at /mnt/photos and has it there.
        // b.jpg: uploaded to Immich before (same content). c.jpg (with a sidecar): new.
        std::fs::write(lib.join("2019/Trip/a.jpg"), b"jpeg a").unwrap();
        std::fs::write(lib.join("2019/Trip/b.jpg"), b"jpeg b").unwrap();
        std::fs::write(lib.join("2019/Trip/c.jpg"), b"jpeg c").unwrap();
        std::fs::write(lib.join("2019/Trip/c.xmp"), b"<x:xmpmeta/>").unwrap();
        let mock: mock::Shared = Default::default();
        mock.lock().assets.push(("ext-a".into(), "/mnt/photos/2019/Trip/a.jpg".into(), "path-hash".into(), "a.jpg".into(), false));
        mock.lock().assets.push(("up-b".into(), "/upload/b.jpg".into(), sha1_b64(b"jpeg b"), "b.jpg".into(), false));
        mock.lock().albums.push(("album-x".into(), "Existing".into(), vec!["up-b".into()]));
        let url = mock::serve(mock.clone()).await;

        let mut cfg = crate::config::Config::for_tests(tmp.path());
        cfg.immich_url = Some(url.clone());
        cfg.immich_api_key = Some("secret".into());
        cfg.immich_library_path = Some("/mnt/photos".into());
        let state = crate::build_state(cfg).await.unwrap();
        let ids: Vec<i64> = {
            let c = state.db.get().unwrap();
            c.execute_batch("INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(1, NULL, '', 'Library', 1), (2, 1, '2019', '2019', 1), (3, 2, '2019/Trip', 'Trip', 1);").unwrap();
            ["a.jpg", "b.jpg", "c.jpg"]
                .iter()
                .map(|n| {
                    let size = std::fs::metadata(lib.join("2019/Trip").join(n)).unwrap().len() as i64;
                    let rec = crate::model::PhotoRec {
                        folder_id: 3,
                        name: n.to_string(),
                        stem: n.trim_end_matches(".jpg").into(),
                        ext: "jpg".into(),
                        kind: crate::model::Kind::Jpeg as i32,
                        size,
                        sidecar: (*n == "c.jpg").then(|| "c.xmp".to_string()),
                        taken: Some(1_560_000_000),
                        ..Default::default()
                    };
                    crate::repo::upsert(&c, &rec).unwrap()
                })
                .collect()
        };
        let app = crate::api::router(state.clone());
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let call = |method: &'static str, uri: &'static str, body: Option<Value>| {
            let app = app.clone();
            async move {
                let req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
                let resp = app.oneshot(req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap()).await.unwrap();
                let st = resp.status();
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
                (st, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
            }
        };
        let (st, s) = call("GET", "/api/status", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(s["immich"]["library"], true);
        let (_, albums) = call("GET", "/api/immich/albums", None).await;
        assert_eq!(albums[0]["name"], "Existing");

        // Into a new album.
        let (st, r) = call("POST", "/api/immich/send", Some(json!({"ids": ids, "album_name": "Trip 2019"}))).await;
        assert_eq!(st, StatusCode::OK, "{r}");
        let job = crate::safety_tests::wait_job(&state, r["job"]["id"].as_str().unwrap()).await;
        assert_eq!(job.failed, 0, "{:?}", job.errors);
        assert!(!job.undo);
        assert_eq!(job.summary.as_deref(), Some("Sent 3 photos to Immich (1 uploaded, 2 already there) · in “Trip 2019”"));
        assert_eq!(job.link.as_deref(), Some(format!("{url}/albums/album-2").as_str()));
        {
            let m = mock.lock();
            assert_eq!(m.uploads, 1, "only the new photo was uploaded");
            assert!(m.assets.iter().any(|a| a.3 == "c.jpg" && a.4), "with its sidecar");
            assert_eq!(m.albums[1].1, "Trip 2019");
            assert_eq!(m.albums[1].2, vec!["ext-a".to_string(), "up-b".into(), "asset-3".into()]);
        }
        // Again, into the existing album: nothing is uploaded, what was there stays once.
        let (_, r) = call("POST", "/api/immich/send", Some(json!({"ids": ids, "album": "album-x"}))).await;
        let job = crate::safety_tests::wait_job(&state, r["job"]["id"].as_str().unwrap()).await;
        assert_eq!(job.failed, 0, "{:?}", job.errors);
        let m = mock.lock();
        assert_eq!(m.uploads, 1);
        assert_eq!(m.albums[0].2.len(), 3);
    }

    #[tokio::test]
    async fn a_bad_key_says_what_it_needs() {
        let mock: mock::Shared = Default::default();
        let url = mock::serve(mock.clone()).await;
        let c = Client::new(&format!("{url}/api"), "wrong");
        let e = tokio::task::spawn_blocking(move || c.albums()).await.unwrap().unwrap_err();
        assert!(format!("{e}").contains("album.read"), "{e}");
    }
}
