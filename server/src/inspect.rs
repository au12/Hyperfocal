//! `hyperfocal inspect [--render DIR] <file>...`: what each decoder makes of a file.
//!
//! For troubleshooting a camera or a single odd file, e.g.
//! `docker exec hyperfocal hyperfocal inspect /photos/2024/Trip/IMG_0001.CR3`.
//! Read-only: it never writes next to the photo (renders go to DIR).

use crate::images::{jpeg, raw, tone};
use crate::meta::{self, FileMeta};
use crate::model::{Kind, kind_of_ext, split_name};
use crate::native;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

pub fn main(args: &[OsString]) -> ExitCode {
    let mut render: Option<PathBuf> = None;
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--render" {
            match it.next() {
                Some(d) => render = Some(PathBuf::from(d)),
                None => return usage(),
            }
        } else {
            files.push(PathBuf::from(a));
        }
    }
    if files.is_empty() {
        return usage();
    }
    if let Some(d) = &render
        && let Err(e) = std::fs::create_dir_all(d)
    {
        eprintln!("hyperfocal inspect: {}: {e}", d.display());
        return ExitCode::from(2);
    }
    for f in &files {
        let v = inspect(f, render.as_deref());
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    }
    ExitCode::SUCCESS
}

fn usage() -> ExitCode {
    eprintln!("usage: hyperfocal inspect [--render DIR] <file>...");
    ExitCode::from(2)
}

fn ms(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

fn meta_json(r: Result<FileMeta, String>, t: Instant) -> Value {
    match r {
        Ok(m) => {
            let mut v = serde_json::to_value(&m).unwrap_or_default();
            if let Some(x) = &m.xmp {
                v["xmp"] = json!({"rating": x.rating, "label": x.label, "pick": x.pick, "keywords": x.subject, "orientation": x.orientation});
            }
            v["ms"] = json!(ms(t));
            v
        }
        Err(e) => json!({ "error": e }),
    }
}

fn dev_json(r: anyhow::Result<raw::Developed>, t: Instant, stem: &str, render: Option<&Path>) -> Value {
    match r {
        Ok(d) => {
            let mut v = json!({
                "decoder": d.decoder, "size": [d.w, d.h], "approximate": d.approximate,
                "clipped": {"r": d.over[0], "g": d.over[1], "b": d.over[2], "any": d.over[3]}, "under": d.under,
                "wb": d.wb, "decode_ms": d.decode_ms, "develop_ms": d.develop_ms, "ms": ms(t),
            });
            if let Some(dir) = render {
                let lut = tone::build_lut(tone::ToneMode::Normal, crate::images::RENDER_BOOST_EV);
                let out = dir.join(format!("{stem}.{}.jpg", d.decoder.to_lowercase()));
                let img = raw::render(&d, &lut, 1600);
                match jpeg::encode(&img, 90, false).map(|b| std::fs::write(&out, b)) {
                    Ok(Ok(())) => v["render"] = json!(out.display().to_string()),
                    Ok(Err(e)) => v["render_error"] = json!(e.to_string()),
                    Err(e) => v["render_error"] = json!(e.to_string()),
                }
            }
            v
        }
        Err(e) => json!({ "error": format!("{e:#}") }),
    }
}

fn inspect(path: &Path, render: Option<&Path>) -> Value {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (stem, ext) = split_name(&name);
    let Some(kind) = kind_of_ext(&ext) else {
        return json!({"file": path.display().to_string(), "error": "not a supported image type"});
    };
    let mut v = json!({"file": path.display().to_string(), "kind": format!("{kind:?}")});
    match kind {
        Kind::Raw => {
            let t = Instant::now();
            v["flashraw"] = meta_json(raw::catch(|| meta::read_raw_meta_flashraw(path)), t);
            let t = Instant::now();
            v["libraw"] = meta_json(meta::read_raw_meta_libraw(path), t);
            let t = Instant::now();
            let fr = raw::catch(|| -> anyhow::Result<raw::Developed> { raw::develop(std::fs::read(path)?, raw::WbMode::Asshot, raw::UE_EV) })
                .map_err(|e| anyhow::anyhow!(e));
            v["develop_flashraw"] = dev_json(fr, t, &stem, render);
            let t = Instant::now();
            v["develop_libraw"] = dev_json(raw::develop_libraw(path, raw::WbMode::Asshot, raw::UE_EV), t, &stem, render);
            let t = Instant::now();
            v["preview"] = match native::libraw_preview(path, 1024) {
                Ok(p) => json!({"libraw_long_edge": p.long_edge(), "ms": ms(t)}),
                Err(e) => json!({"libraw_error": format!("{e:#}")}),
            };
        }
        Kind::Heic => {
            let t = Instant::now();
            v["metadata"] = meta_json(meta::read_file_meta(path, kind), t);
            let t = Instant::now();
            v["decode"] = match native::heic_decode(path, 0) {
                Ok((img, info)) => {
                    let mut d = json!({"size": [img.w, img.h], "bits": info.bits, "color": info.color, "ms": ms(t)});
                    if let Some(dir) = render {
                        let out = dir.join(format!("{stem}.heic.jpg"));
                        if let Ok(b) = jpeg::fit_long(img, 1600).and_then(|i| jpeg::encode(&i, 90, false))
                            && std::fs::write(&out, b).is_ok()
                        {
                            d["render"] = json!(out.display().to_string());
                        }
                    }
                    d
                }
                Err(e) => json!({"error": format!("{e:#}")}),
            };
        }
        _ => {
            let t = Instant::now();
            v["metadata"] = meta_json(meta::read_file_meta(path, kind), t);
        }
    }
    v
}
