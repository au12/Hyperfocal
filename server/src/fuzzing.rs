//! Entry points for fuzzing (`server/fuzz`, cargo-fuzz): each runs the parsers of one
//! kind of untrusted input over arbitrary bytes. Errors are fine; a panic, a hang or an
//! unbounded allocation is a bug. The smoke test below runs every entry point over
//! mutated copies of the test fixtures on stable Rust, as part of `cargo test`.

use std::io::{BufReader, Cursor};
use xxhash_rust::xxh3::Xxh3;

/// HEIF/HEIC metadata (EXIF, XMP, size, rotation) and the image items the integrity
/// monitor hashes.
pub fn heif(data: &[u8]) {
    let len = data.len() as u64;
    let _ = crate::meta::heif::read_from(&mut Cursor::new(data), len);
    let _ = crate::meta::heif::image_parts(&mut Cursor::new(data), len, &mut |_| {});
}

/// An XMP packet (a sidecar or one embedded in a photo), and the value parsers it uses.
pub fn xmp(data: &[u8]) {
    let x = crate::meta::xmp::Xmp::parse(data);
    let _ = x.is_empty();
    let s = String::from_utf8_lossy(data);
    let _ = crate::meta::xmp::parse_coord(&s);
    let _ = crate::meta::xmp::label_color(&s);
}

/// JPEG headers (EXIF, XMP, size) and the image data the integrity monitor hashes.
pub fn jpeg(data: &[u8]) {
    let _ = crate::meta::read_jpeg_headers(&mut Cursor::new(data));
    let _ = crate::integrity::jpeg_image(data, &mut Xxh3::new());
}

/// TIFF/DNG main-image data (strips and tiles) and Canon CR3 white levels.
pub fn tiff(data: &[u8]) {
    let _ = crate::integrity::tiff_parts(&mut Cursor::new(data), data.len() as u64, &mut Xxh3::new());
    let _ = crate::images::canon::white_levels(data);
}

/// The native helper's output: the framing and every response decoder.
pub fn helper(data: &[u8]) {
    if let Ok(f) = crate::native::frames::Frames::parse(data) {
        for tag in [b"INFO", b"DATA", b"RGB8", b"XMP "] {
            let _ = f.get(tag);
        }
    }
    let _ = crate::native::frames::u16s(data);
    let _ = crate::native::parse_heic(data);
    let _ = crate::native::parse_libraw_meta(data);
    let _ = crate::native::parse_libraw_preview(data);
    let _ = crate::native::parse_libraw_raw(data);
}

/// GPS tracks (GPX, KML, TCX, NMEA, phone location history) and their time stamps.
pub fn track(data: &[u8]) {
    let _ = crate::tracks::parse(BufReader::new(data));
    let s = String::from_utf8_lossy(data);
    let _ = crate::tracks::parse_time(&s);
    let _ = crate::tracks::zone_of(&s);
}

/// Every entry point, for the smoke test.
pub const ALL: [(&str, fn(&[u8])); 6] = [("heif", heif), ("xmp", xmp), ("jpeg", jpeg), ("tiff", tiff), ("helper", helper), ("track", track)];

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator (xorshift), so failures reproduce.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// Mutations in the spirit of a fuzzer: flipped bits, overwritten bytes with
    /// interesting values, truncation, duplicated and removed runs.
    fn mutate(rng: &mut Rng, src: &[u8]) -> Vec<u8> {
        let mut v = src.to_vec();
        for _ in 0..1 + rng.below(8) {
            if v.is_empty() {
                v.push(rng.next() as u8);
                continue;
            }
            let i = rng.below(v.len());
            match rng.below(6) {
                0 => v[i] ^= 1 << rng.below(8),
                1 => v[i] = [0, 0xff, 0x7f, 0x80, 1][rng.below(5)],
                2 => v.truncate(i),
                3 => {
                    let n = rng.below(64).min(v.len() - i);
                    let run = v[i..i + n].to_vec();
                    let at = rng.below(v.len());
                    v.splice(at..at, run);
                }
                4 => {
                    let n = rng.below(64).min(v.len() - i);
                    v.drain(i..i + n);
                }
                _ => {
                    // A length field: four bytes of a big or small number.
                    let n = [0u32, 1, 0xffff_ffff, 0x7fff_ffff, 0x10, src.len() as u32][rng.below(6)];
                    let b = if rng.below(2) == 0 { n.to_le_bytes() } else { n.to_be_bytes() };
                    for (k, x) in b.iter().enumerate() {
                        if let Some(d) = v.get_mut(i + k) {
                            *d = *x;
                        }
                    }
                }
            }
        }
        v
    }

    fn seeds() -> Vec<Vec<u8>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut out: Vec<Vec<u8>> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x != "py"))
            .map(|e| std::fs::read(e.path()).unwrap())
            .collect();
        // Text inputs: a sidecar, tracks, helper output.
        out.push(br#"<?xpacket?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:exif="http://ns.adobe.com/exif/1.0/" xmp:Rating="3" xmp:Label="Red" exif:GPSLatitude="38,43.4N" exif:GPSLongitude="9,8.4W"><dc:subject><rdf:Bag><rdf:li>a|b</rdf:li></rdf:Bag></dc:subject></rdf:Description></rdf:RDF></x:xmpmeta>"#.to_vec());
        out.push(br#"<gpx><trk><trkseg><trkpt lat="38.7" lon="-9.1"><ele>20</ele><time>2024-05-01T10:00:00Z</time></trkpt><trkpt lat="38.71" lon="-9.12"><time>2024-05-01T10:05:00+01:00</time></trkpt></trkseg></trk></gpx>"#.to_vec());
        out.push(b"$GPRMC,100000,A,3842.000,N,00908.000,W,0.0,0.0,010524,,*00\n$GPGGA,100100,3842.010,N,00908.010,W,1,8,1.0,20.0,M,,M,,*00\n".to_vec());
        out.push(r#"{"locations":[{"latitudeE7":387000000,"longitudeE7":-91000000,"timestamp":"2024-05-01T10:00:00Z"}],"semanticSegments":[{"startTime":"2024-05-01T09:00:00Z","endTime":"2024-05-01T11:00:00Z","visit":{"topCandidate":{"placeLocation":{"latLng":"38.7°, -9.1°"}}}}]}"#.as_bytes().to_vec());
        let mut frames = Vec::new();
        crate::native::frames::begin(&mut frames).unwrap();
        crate::native::frames::json(&mut frames, b"INFO", &serde_json::json!({"w": 2, "h": 1, "source": "decoded"})).unwrap();
        crate::native::frames::frame(&mut frames, b"RGB8", &[1, 2, 3, 4, 5, 6]).unwrap();
        out.push(frames);
        out
    }

    #[test]
    fn parsers_survive_mutated_inputs() {
        let seeds = seeds();
        assert!(seeds.len() >= 8);
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let rounds = if cfg!(debug_assertions) { 300 } else { 3000 };
        for seed in &seeds {
            for (_, f) in ALL {
                f(seed);
            }
            for _ in 0..rounds {
                let input = mutate(&mut rng, seed);
                for (_, f) in ALL {
                    f(&input);
                }
            }
        }
        // Pure noise and edge sizes.
        for n in [0, 1, 2, 3, 4, 7, 8, 12, 16, 64, 4096] {
            let input: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            for (_, f) in ALL {
                f(&input);
            }
        }
    }
}
