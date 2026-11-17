//! Decoder tests against the fixtures in tests/fixtures (regenerate them with
//! tests/fixtures/make_fixtures.py). The ops run in-process here; the spawned
//! helper is covered by tests/decoders.rs and the process handling below.
// Builds without both decoders only run the process tests.
#![cfg_attr(not(all(feature = "heic", feature = "libraw")), allow(unused_imports, dead_code))]

use super::testing;
use super::*;
use crate::images::raw;
use crate::meta;
use crate::model::Kind;
use std::time::Instant;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn px(img: &Rgb, x: usize, y: usize) -> [u8; 3] {
    let i = (y * img.w + x) * 3;
    [img.px[i], img.px[i + 1], img.px[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 3], tol: i32) -> bool {
    a.iter().zip(b).all(|(x, y)| (*x as i32 - y as i32).abs() <= tol)
}

#[cfg(feature = "heic")]
mod heic {
    use super::*;

    #[test]
    fn rotation_is_applied_once() {
        // Stored 96x64 with red on the left; container rotation 90° clockwise, and
        // EXIF orientation 6 as well (like an iPhone). Shown 64x96 with red on top.
        let (img, info) = testing::heic_decode(&fixture("rotated.heic"), 0).unwrap();
        assert_eq!((img.w, img.h, info.src_w, info.src_h), (64, 96, 64, 96));
        assert!(near(px(&img, 32, 10), [230, 20, 20], 12), "{:?}", px(&img, 32, 10));
        assert!(near(px(&img, 32, 85), [20, 20, 230], 12), "{:?}", px(&img, 32, 85));
        // The index agrees: same size, and no orientation left to apply.
        let m = meta::read_file_meta(&fixture("rotated.heic"), Kind::Heic).unwrap();
        assert_eq!((m.width, m.height, m.orientation), (Some(64), Some(96), 1));
        assert_eq!(m.make.as_deref(), Some("Apple"));
        assert_eq!(m.taken.as_deref(), Some("2024-07-04T18:30:05+01:00"));
        assert!((m.lat.unwrap() - 38.7223).abs() < 1e-4 && (m.lon.unwrap() + 9.1393).abs() < 1e-4);
        let x = m.xmp.expect("embedded XMP");
        assert_eq!((x.rating, x.orientation), (Some(4), None));
    }

    #[test]
    fn scaled() {
        let (img, info) = testing::heic_decode(&fixture("rotated.heic"), 48).unwrap();
        assert_eq!((img.w, img.h), (32, 48));
        assert_eq!((info.w, info.h), (32, 48));
    }

    #[test]
    fn display_p3_becomes_srgb() {
        // P3 (200, 100, 50) is sRGB (215, 93, 31).
        let (img, info) = testing::heic_decode(&fixture("display-p3.heic"), 0).unwrap();
        assert_eq!(info.color, "Display P3");
        assert!(near(px(&img, 16, 16), [215, 93, 31], 3), "{:?}", px(&img, 16, 16));
    }

    #[test]
    fn hdr_pq_is_tone_mapped() {
        let (img, info) = testing::heic_decode(&fixture("pq-hdr.heic"), 0).unwrap();
        assert_eq!(info.bits, 10);
        assert!(info.color.contains("PQ"), "{}", info.color);
        // 203-nit reference white lands just below white; 1000 nits is white.
        let top = px(&img, 16, 4);
        assert!(top.iter().all(|v| (230..=248).contains(v)), "{top:?}");
        assert_eq!(px(&img, 16, 28), [255, 255, 255]);
    }

    #[test]
    fn not_heif() {
        assert!(testing::heic_decode(&fixture("tiny.dng"), 0).is_err());
        assert!(testing::heic_decode(&fixture("missing.heic"), 0).is_err());
    }
}

#[cfg(feature = "libraw")]
mod libraw {
    use super::*;

    #[test]
    fn metadata() {
        let m = testing::libraw_meta(&fixture("tiny.dng")).unwrap();
        assert_eq!((m.make.as_deref(), m.model.as_deref()), (Some("Hyperfocal"), Some("Test DNG")));
        assert_eq!((m.width, m.height, m.orientation), (64, 48, 6));
        assert_eq!(m.taken.as_deref(), Some("2024-07-04T18:30:05"));
        assert_eq!((m.iso, m.fnum, m.exposure), (Some(200), Some(2.8), Some(0.004)));
        // Same as FlashRaw.
        let f = meta::read_raw_meta_flashraw(&fixture("tiny.dng")).unwrap();
        assert_eq!((f.width, f.height, f.orientation, f.taken.as_deref()), (Some(64), Some(48), 6, Some("2024-07-04T18:30:05")));
    }

    #[test]
    fn sensor_data() {
        let LibrawRaw::Bayer(info, data) = testing::libraw_raw(&fixture("tiny.dng"), "asshot").unwrap() else {
            panic!("expected Bayer data");
        };
        assert_eq!((info.w, info.h, info.ch), (64, 48, [[0, 1], [1, 2]]));
        assert!(info.black.iter().all(|b| *b == 256.0), "{:?}", info.black);
        assert_eq!(info.white, 4095.0);
        let wb = info.as_shot.unwrap();
        assert!((wb[0] - 2.0).abs() < 1e-3 && (wb[2] - 1.5).abs() < 1e-3, "{wb:?}");
        // The samples arrive untouched: R at (0,0), G at (1,0); the clipped corner.
        assert_eq!(data[0], 256 + ((4095.0 - 256.0) * 0.25 * 0.5f64).round() as u16);
        assert_eq!(data[1], 256 + ((4095.0 - 256.0) * 0.25f64).round() as u16);
        assert_eq!(data[63], 4095);
    }

    #[test]
    fn develops_like_flashraw() {
        let flash = raw::develop(std::fs::read(fixture("tiny.dng")).unwrap(), raw::WbMode::Asshot, 8.0).unwrap();
        let lib = raw::develop_libraw_data(testing::libraw_raw(&fixture("tiny.dng"), "asshot").unwrap(), raw::WbMode::Asshot, 8.0, Instant::now()).unwrap();
        assert_eq!((flash.decoder, lib.decoder), ("FlashRaw", "LibRaw"));
        assert_eq!((flash.w, flash.h), (lib.w, lib.h));
        assert_eq!(flash.over, lib.over);
        assert_eq!(flash.mask, lib.mask);
        assert!(!lib.approximate);
        // The grey half is neutral and at 25% of the range.
        let t = raw::f16_table();
        let at = |d: &raw::Developed, x: usize, y: usize| -> [f32; 3] {
            let i = (y * d.w as usize + x) * 3;
            [t[d.rgb[i] as usize], t[d.rgb[i + 1] as usize], t[d.rgb[i + 2] as usize]]
        };
        for d in [&flash, &lib] {
            let [r, g, b] = at(d, 4, 12);
            assert!((g - 0.25).abs() < 0.01 && (r - g).abs() < 0.01 && (b - g).abs() < 0.01, "{} {r} {g} {b}", d.decoder);
        }
        // Green clipped on the bright half, every channel in the corner.
        assert_eq!(lib.mask[12 * lib.w as usize + 20] & 7, 2);
        assert_eq!(lib.mask[lib.w as usize - 1] & 7, 7);
    }

    #[test]
    fn processed_by_libraw() {
        // Daylight balance and LibRaw's own development path (used for X-Trans and the like).
        let LibrawRaw::Bayer(info, _) = testing::libraw_raw(&fixture("tiny.dng"), "daylight").unwrap() else { panic!() };
        assert!(info.daylight.is_some());
    }

    #[test]
    fn errors_are_clean() {
        assert!(testing::libraw_meta(&fixture("rotated.heic")).is_err());
        assert!(testing::libraw_meta(&fixture("missing.dng")).is_err());
        assert!(testing::libraw_preview(&fixture("tiny.dng"), 512).is_err());
        assert!(testing::libraw_raw(&fixture("display-p3.heic"), "asshot").is_err());
    }
}

/// The parent side survives whatever the helper does.
mod process {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn crash_hang_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let crash = script(dir.path(), "crash", "echo partial; kill -SEGV $$");
        let e = run_with(&crash, vec!["heic".into()], Duration::from_secs(10)).unwrap_err().to_string();
        assert!(e.contains("crashed (signal 11)"), "{e}");

        let hang = script(dir.path(), "hang", "exec sleep 30");
        let t = Instant::now();
        let e = run_with(&hang, vec![], Duration::from_millis(300)).unwrap_err().to_string();
        assert!(e.contains("was stopped") && t.elapsed() < Duration::from_secs(5), "{e}");

        let garbage = script(dir.path(), "garbage", "echo running 3 tests");
        let out = run_with(&garbage, vec![], Duration::from_secs(10)).unwrap();
        assert!(parse(&out).is_err());

        let missing = dir.path().join("does-not-exist");
        assert!(run_with(&missing, vec![], Duration::from_secs(1)).is_err());
    }

    #[test]
    fn unit_tests_never_spawn_themselves() {
        assert!(helper_exe().is_err() || std::env::var_os("HYPERFOCAL_HELPER_EXE").is_some());
    }
}
