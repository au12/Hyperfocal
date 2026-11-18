//! Core records shared by the indexer, the API and the write path.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[repr(i32)]
pub enum Kind {
    Raw = 1,
    Jpeg = 2,
    Tiff = 3,
    Png = 4,
    Heic = 5,
}

impl Kind {
    pub fn from_i32(v: i32) -> Kind {
        match v {
            2 => Kind::Jpeg,
            3 => Kind::Tiff,
            4 => Kind::Png,
            5 => Kind::Heic,
            _ => Kind::Raw,
        }
    }
}

pub const RAW_EXTS: &[&str] = &[
    "cr2", "cr3", "crw", "dng", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "x3f", "3fr",
    "iiq", "erf", "mos", "mrw", "kdc", "dcr", "rwl", "gpr", "raw",
];

pub fn kind_of_ext(ext: &str) -> Option<Kind> {
    let e = ext.to_ascii_lowercase();
    if RAW_EXTS.contains(&e.as_str()) {
        return Some(Kind::Raw);
    }
    match e.as_str() {
        "jpg" | "jpeg" | "jpe" => Some(Kind::Jpeg),
        "tif" | "tiff" => Some(Kind::Tiff),
        "png" => Some(Kind::Png),
        "heic" | "heif" | "hif" => Some(Kind::Heic),
        _ => None,
    }
}

/// Split a file name into (stem, lowercase extension).
pub fn split_name(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i + 1..].to_ascii_lowercase()),
        _ => (name.to_string(), String::new()),
    }
}

/// Everything the index knows about one photo (effective values: embedded EXIF,
/// overridden by embedded XMP, overridden by the XMP sidecar).
#[derive(Debug, Clone, Default)]
pub struct PhotoRec {
    pub id: i64,
    pub folder_id: i64,
    pub name: String,
    pub stem: String,
    pub ext: String,
    pub kind: i32,
    pub size: i64,
    pub mtime: i64,
    pub sidecar: Option<String>,
    pub sidecar_mtime: Option<i64>,
    pub companion: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: i32,
    pub taken: Option<i64>,
    pub taken_str: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub focal: Option<f64>,
    pub fnum: Option<f64>,
    pub exposure: Option<f64>,
    pub iso: Option<i64>,
    pub bias: Option<f64>,
    pub rating: i32,
    pub label: Option<String>,
    pub pick: i32,
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    pub rights: Option<String>,
    pub keywords: Vec<String>,
    pub hkeywords: Vec<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt: Option<f64>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub sublocation: Option<String>,
    pub meta_err: Option<String>,
}

/// Seconds since the epoch for a capture time, treating the camera's local clock as UTC
/// (consistent ordering across a library shot in many time zones).
pub fn taken_to_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 10 {
        return None;
    }
    let num = |a: usize, n: usize| -> Option<i64> {
        std::str::from_utf8(b.get(a..a + n)?).ok()?.parse::<i64>().ok()
    };
    let y = num(0, 4)?;
    let mo = num(5, 2)?;
    let d = num(8, 2)?;
    let (h, mi, sec) = if b.len() >= 19 { (num(11, 2)?, num(14, 2)?, num(17, 2)?) } else { (0, 0, 0) };
    if !(1800..=2200).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec)
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's days_from_civil).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The civil date (year, month, day) of a day number since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn civil_dates() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (1969, 12, 31), (2024, 7, 4), (1850, 3, 1)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m as u32, d as u32));
        }
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
    }

    #[test]
    fn taken() {
        assert_eq!(taken_to_secs("1970-01-01T00:00:00"), Some(0));
        assert_eq!(taken_to_secs("2021-05-12T12:18:20.45+02:00"), Some(1620821900));
        assert_eq!(taken_to_secs("2013:06:08 11:14:40"), Some(1370690080));
        assert_eq!(taken_to_secs("garbage"), None);
    }
    #[test]
    fn names() {
        assert_eq!(split_name("IMG_0001.CR3"), ("IMG_0001".into(), "cr3".into()));
        assert_eq!(split_name(".hidden"), (".hidden".into(), "".into()));
        assert_eq!(kind_of_ext("DNG"), Some(Kind::Raw));
        assert_eq!(kind_of_ext("xmp"), None);
    }
}
