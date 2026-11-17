//! End-to-end through the real binary: `hyperfocal inspect` reads the fixtures with
//! every decoder, spawning the native helper exactly as the server does.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name).display().to_string()
}

fn inspect(files: &[&str]) -> Vec<Value> {
    let out = Command::new(env!("CARGO_BIN_EXE_hyperfocal")).arg("inspect").args(files.iter().map(|f| fixture(f))).output().expect("runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::Deserializer::from_slice(&out.stdout).into_iter::<Value>().map(|v| v.expect("JSON")).collect()
}

#[test]
fn raw_with_both_decoders() {
    let v = &inspect(&["tiny.dng"])[0];
    for k in ["flashraw", "libraw"] {
        assert_eq!(v[k]["make"], "Hyperfocal", "{k}: {}", v[k]);
        assert_eq!(v[k]["orientation"], 6, "{k}");
    }
    assert_eq!(v["develop_flashraw"]["decoder"], "FlashRaw");
    assert_eq!(v["develop_libraw"]["decoder"], "LibRaw", "{}", v["develop_libraw"]);
    assert_eq!(v["develop_libraw"]["size"], v["develop_flashraw"]["size"]);
    assert_eq!(v["develop_libraw"]["clipped"], v["develop_flashraw"]["clipped"]);
}

#[test]
fn heic_through_the_helper() {
    let all = inspect(&["rotated.heic", "pq-hdr.heic"]);
    let (rot, pq) = (&all[0], &all[1]);
    assert_eq!(rot["decode"]["size"], serde_json::json!([64, 96]), "{}", rot["decode"]);
    assert_eq!(rot["metadata"]["orientation"], 1);
    assert_eq!(rot["metadata"]["width"], 64);
    assert_eq!(pq["decode"]["bits"], 10);
}

#[test]
fn helper_reports_errors_as_frames() {
    let out = Command::new(env!("CARGO_BIN_EXE_hyperfocal")).args(["native", "libraw-meta", "/nonexistent/file.CR3"]).output().expect("runs");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.starts_with(b"HFNATIV1"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("ERR "));
    let out = Command::new(env!("CARGO_BIN_EXE_hyperfocal")).args(["native", "no-such-op"]).output().expect("runs");
    assert_eq!(out.status.code(), Some(2));
}
