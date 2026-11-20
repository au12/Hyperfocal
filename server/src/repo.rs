//! Row mapping and index maintenance for photos.

use crate::meta::lcolor;
use crate::model::PhotoRec;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};

pub const PHOTO_COLS: &str = "p.id, p.folder_id, p.name, p.stem, p.ext, p.kind, p.size, p.mtime, p.sidecar, p.sidecar_mtime, \
    p.pair_of, p.companion, p.width, p.height, p.orientation, p.taken, p.taken_str, p.make, p.model, p.lens, \
    p.focal, p.fnum, p.exposure, p.iso, p.bias, p.rating, p.label, p.pick, p.title, p.description, p.creator, \
    p.rights, p.keywords, p.hkeywords, p.lat, p.lon, p.alt, p.city, p.state, p.country, p.country_code, \
    p.sublocation, p.meta_err";

fn split_list(s: Option<String>) -> Vec<String> {
    s.map(|s| s.split('\n').filter(|x| !x.is_empty()).map(|x| x.to_string()).collect()).unwrap_or_default()
}

pub fn photo_from_row(r: &Row) -> rusqlite::Result<PhotoRec> {
    Ok(PhotoRec {
        id: r.get(0)?,
        folder_id: r.get(1)?,
        name: r.get(2)?,
        stem: r.get(3)?,
        ext: r.get(4)?,
        kind: r.get(5)?,
        size: r.get(6)?,
        mtime: r.get(7)?,
        sidecar: r.get(8)?,
        sidecar_mtime: r.get(9)?,
        // 10: pair_of, kept up to date by `regroup` and used in queries only.
        companion: r.get(11)?,
        width: r.get(12)?,
        height: r.get(13)?,
        orientation: r.get(14)?,
        taken: r.get(15)?,
        taken_str: r.get(16)?,
        make: r.get(17)?,
        model: r.get(18)?,
        lens: r.get(19)?,
        focal: r.get(20)?,
        fnum: r.get(21)?,
        exposure: r.get(22)?,
        iso: r.get(23)?,
        bias: r.get(24)?,
        rating: r.get(25)?,
        label: r.get(26)?,
        pick: r.get(27)?,
        title: r.get(28)?,
        description: r.get(29)?,
        creator: r.get(30)?,
        rights: r.get(31)?,
        keywords: split_list(r.get(32)?),
        hkeywords: split_list(r.get(33)?),
        lat: r.get(34)?,
        lon: r.get(35)?,
        alt: r.get(36)?,
        city: r.get(37)?,
        state: r.get(38)?,
        country: r.get(39)?,
        country_code: r.get(40)?,
        sublocation: r.get(41)?,
        meta_err: r.get(42)?,
        regions: None,
        persons: Vec::new(),
    })
}

/// A photo plus where it lives.
#[derive(Debug, Clone)]
pub struct Located {
    pub rec: PhotoRec,
    pub folder_path: String,
    pub folder_is_reject: bool,
    pub folder_parent: Option<i64>,
}

pub fn load(c: &Connection, id: i64) -> Result<Option<Located>> {
    let sql = format!(
        "SELECT {PHOTO_COLS}, f.path, f.is_reject, f.parent_id FROM photos p JOIN folders f ON f.id = p.folder_id WHERE p.id = ?1"
    );
    Ok(c.query_row(&sql, [id], |r| {
        Ok(Located {
            rec: photo_from_row(r)?,
            folder_path: r.get(43)?,
            folder_is_reject: r.get::<_, i64>(44)? != 0,
            folder_parent: r.get(45)?,
        })
    })
    .optional()?)
}

pub fn upsert(c: &Connection, rec: &PhotoRec) -> Result<i64> {
    let lc = lcolor(rec.label.as_deref());
    let id: i64 = c.query_row(
        "INSERT INTO photos(folder_id, name, stem, ext, kind, size, mtime, sidecar, sidecar_mtime, width, height,
            orientation, taken, taken_str, make, model, lens, focal, fnum, exposure, iso, bias, rating, label, lcolor,
            pick, title, description, creator, rights, keywords, hkeywords, lat, lon, alt, city, state, country,
            country_code, sublocation, meta_err, indexed_at, added_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,
            ?29,?30,?31,?32,?33,?34,?35,?36,?37,?38,?39,?40,?41,?42,?42)
         ON CONFLICT(folder_id, name) DO UPDATE SET stem=excluded.stem, ext=excluded.ext, kind=excluded.kind,
            size=excluded.size, mtime=excluded.mtime, sidecar=excluded.sidecar, sidecar_mtime=excluded.sidecar_mtime,
            width=excluded.width, height=excluded.height, orientation=excluded.orientation, taken=excluded.taken,
            taken_str=excluded.taken_str, make=excluded.make, model=excluded.model, lens=excluded.lens,
            focal=excluded.focal, fnum=excluded.fnum, exposure=excluded.exposure, iso=excluded.iso, bias=excluded.bias,
            rating=excluded.rating, label=excluded.label, lcolor=excluded.lcolor, pick=excluded.pick,
            title=excluded.title, description=excluded.description, creator=excluded.creator, rights=excluded.rights,
            keywords=excluded.keywords, hkeywords=excluded.hkeywords, lat=excluded.lat, lon=excluded.lon,
            alt=excluded.alt, city=excluded.city, state=excluded.state, country=excluded.country,
            country_code=excluded.country_code, sublocation=excluded.sublocation, meta_err=excluded.meta_err,
            indexed_at=excluded.indexed_at
         RETURNING id",
        params![
            rec.folder_id,
            rec.name,
            rec.stem,
            rec.ext,
            rec.kind,
            rec.size,
            rec.mtime,
            rec.sidecar,
            rec.sidecar_mtime,
            rec.width,
            rec.height,
            rec.orientation,
            rec.taken,
            rec.taken_str,
            rec.make,
            rec.model,
            rec.lens,
            rec.focal,
            rec.fnum,
            rec.exposure,
            rec.iso,
            rec.bias,
            rec.rating,
            rec.label,
            lc,
            rec.pick,
            rec.title,
            rec.description,
            rec.creator,
            rec.rights,
            join_list(&rec.keywords),
            join_list(&rec.hkeywords),
            rec.lat,
            rec.lon,
            rec.alt,
            rec.city,
            rec.state,
            rec.country,
            rec.country_code,
            rec.sublocation,
            rec.meta_err,
            crate::db::now_ms(),
        ],
        |r| r.get(0),
    )?;
    Ok(id)
}

fn join_list(v: &[String]) -> Option<String> {
    if v.is_empty() { None } else { Some(v.join("\n")) }
}

/// The text substring search looks through: every searchable field, one per line (a
/// search word never spans two fields).
fn search_text(rec: &PhotoRec, folder_path: &str) -> String {
    let mut t = String::with_capacity(160);
    let mut field = |v: &str| {
        if !v.is_empty() {
            t.push_str(v);
            t.push('\n');
        }
    };
    field(&rec.name);
    field(folder_path);
    for v in [&rec.title, &rec.description, &rec.sublocation, &rec.city, &rec.state, &rec.country, &rec.make, &rec.model, &rec.lens] {
        field(v.as_deref().unwrap_or(""));
    }
    for k in rec.keywords.iter().chain(rec.hkeywords.iter()) {
        field(k);
    }
    t
}

/// Refresh the trigram (substring search) row of one photo.
pub fn index_trigram(c: &Connection, id: i64, rec: &PhotoRec, folder_path: &str) -> Result<()> {
    c.prepare_cached("DELETE FROM photos_tri WHERE rowid = ?1")?.execute([id])?;
    c.prepare_cached("INSERT INTO photos_tri(rowid, t) VALUES(?1, ?2)")?.execute(params![id, search_text(rec, folder_path)])?;
    Ok(())
}

/// Refresh the full-text indexes and keyword table for one photo.
pub fn index_text(c: &Connection, id: i64, rec: &PhotoRec, folder_path: &str) -> Result<()> {
    index_trigram(c, id, rec, folder_path)?;
    c.execute("DELETE FROM photos_fts WHERE rowid = ?1", [id])?;
    let place = [&rec.sublocation, &rec.city, &rec.state, &rec.country]
        .iter()
        .filter_map(|v| v.as_deref())
        .collect::<Vec<_>>()
        .join(" ");
    let camera = [&rec.make, &rec.model, &rec.lens].iter().filter_map(|v| v.as_deref()).collect::<Vec<_>>().join(" ");
    let mut kws: Vec<String> = rec.keywords.clone();
    for h in &rec.hkeywords {
        kws.extend(h.split('|').map(|s| s.to_string()));
    }
    c.execute(
        "INSERT INTO photos_fts(rowid, name, path, title, description, keywords, place, camera) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            id,
            rec.name,
            folder_path.replace('/', " "),
            rec.title.as_deref().unwrap_or(""),
            rec.description.as_deref().unwrap_or(""),
            kws.join(" "),
            place,
            camera
        ],
    )?;
    c.execute("DELETE FROM photo_keywords WHERE photo_id = ?1", [id])?;
    let mut seen = std::collections::HashSet::new();
    let mut stmt = c.prepare_cached("INSERT OR IGNORE INTO photo_keywords(photo_id, kw) VALUES(?1, ?2)")?;
    for k in rec.keywords.iter().chain(rec.hkeywords.iter()) {
        let k = k.trim();
        if !k.is_empty() && seen.insert(k.to_lowercase()) {
            stmt.execute(params![id, k])?;
        }
    }
    Ok(())
}

pub fn delete_photo(c: &Connection, id: i64) -> Result<()> {
    c.execute("DELETE FROM photos_fts WHERE rowid = ?1", [id])?;
    c.execute("DELETE FROM photos_tri WHERE rowid = ?1", [id])?;
    c.execute("DELETE FROM photos WHERE id = ?1", [id])?;
    Ok(())
}

/// Fold JPEG/HEIF files into their RAW (`pair_of`, and the RAW's `companion` list),
/// following `sets`. Writes only what differs and returns whether anything did.
pub fn regroup(c: &Connection, folder_id: i64) -> Result<bool> {
    let rows: Vec<(i64, String, Option<i64>, Option<String>)> = {
        let mut st = c.prepare("SELECT id, name, pair_of, companion FROM photos WHERE folder_id = ?1")?;
        let it = st.query_map([folder_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        it.collect::<rusqlite::Result<_>>()?
    };
    let listing = crate::sets::Listing::new(rows.iter().map(|r| r.1.clone()).collect(), Vec::new());
    let plan = crate::sets::Plan::new(&listing);
    let id_of: std::collections::HashMap<&str, i64> = rows.iter().map(|r| (r.1.as_str(), r.0)).collect();
    let mut changed = false;
    for (id, name, pair_of, companion) in &rows {
        let want_pair = match plan.role(name) {
            Some(crate::sets::Role::Paired { raw }) => id_of.get(raw.as_str()).copied(),
            _ => None,
        };
        let paired = plan.paired(name);
        let want_companion = (!paired.is_empty()).then(|| paired.join(","));
        if want_pair != *pair_of {
            c.execute("UPDATE photos SET pair_of = ?1 WHERE id = ?2", params![want_pair, id])?;
            changed = true;
        }
        if want_companion != *companion {
            c.execute("UPDATE photos SET companion = ?1 WHERE id = ?2", params![want_companion, id])?;
            changed = true;
        }
    }
    if changed {
        update_count(c, folder_id)?;
    }
    Ok(changed)
}

fn update_count(c: &Connection, folder_id: i64) -> Result<()> {
    c.execute(
        "UPDATE folders SET photo_count = (SELECT count(*) FROM photos WHERE folder_id = ?1 AND pair_of IS NULL) WHERE id = ?1",
        [folder_id],
    )?;
    Ok(())
}

/// Recompute the pairs and the visible photo count of a folder after its files changed.
pub fn refresh_folder(c: &Connection, folder_id: i64) -> Result<()> {
    regroup(c, folder_id)?;
    update_count(c, folder_id)?;
    c.execute("UPDATE folders SET scanned_at = ?2 WHERE id = ?1", params![folder_id, crate::db::now_ms()])?;
    Ok(())
}
