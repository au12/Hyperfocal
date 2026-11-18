//! GPS tracks for geotagging: GPX, KML, TCX, NMEA and Google's location history
//! exports (Takeout `Records.json` and monthly semantic files, the phone's
//! `Timeline.json`, the iPhone export) read into one time-ordered list of fixes, and
//! the position of a moment on that list.
//!
//! Every reader streams: a location history of a gigabyte is read without holding
//! more than its fixes in memory.

use anyhow::{Result, bail};
use quick_xml::events::{BytesStart, Event};
use serde::Deserialize;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::fmt;
use std::io::BufRead;
use std::marker::PhantomData;

/// One position at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fix {
    /// Milliseconds since the epoch, UTC.
    pub t: i64,
    pub lat: f64,
    pub lon: f64,
    /// Metres above sea level; NaN when unknown.
    pub ele: f32,
    /// The person stayed here until the next fix (a visit in a location history), so
    /// any moment up to it is here, however long the gap.
    pub stay: bool,
}

#[derive(Debug)]
pub struct Track {
    pub format: &'static str,
    /// Sorted by time, at most one per millisecond.
    pub fixes: Vec<Fix>,
}

/// Fixes less accurate than this (metres) are left out: a cell tower's guess places a
/// photo kilometres away.
const MAX_ACCURACY_M: f64 = 1000.0;

fn fix(t: i64, lat: f64, lon: f64, ele: f32) -> Option<Fix> {
    let ok = lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0 && !(lat == 0.0 && lon == 0.0);
    // Years 1980 to 2200: anything else is a parse error.
    (ok && (315_532_800_000..7_258_118_400_000).contains(&t)).then_some(Fix { t, lat, lon, ele, stay: false })
}

/// A stay at one place from `begin` to `end` (either may be unknown).
fn stay(out: &mut Vec<Fix>, begin: Option<i64>, end: Option<i64>, lat: f64, lon: f64, ele: f32) {
    if let Some(b) = begin {
        out.extend(fix(b, lat, lon, ele).map(|f| Fix { stay: end.is_some_and(|e| e > b), ..f }));
    }
    if let Some(e) = end {
        out.extend(fix(e, lat, lon, ele));
    }
}

/// Time order, one fix per millisecond; a stay that begins at a moment another fix
/// shares is kept.
fn tidy(v: &mut Vec<Fix>) {
    v.sort_by_key(|f| f.t);
    v.dedup_by(|later, kept| {
        let same = later.t == kept.t;
        if same {
            kept.stay |= later.stay;
        }
        same
    });
}

/// Read a track file of any supported format.
pub fn parse<R: BufRead>(mut r: R) -> Result<Track> {
    let head = r.fill_buf()?;
    let bom = if head.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
    let first = head[bom..].iter().copied().find(|b| !b.is_ascii_whitespace());
    if head.starts_with(b"PK\x03\x04") {
        bail!("this is a zip archive: unzip it first, then choose the .gpx, .kml or .json file inside");
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        bail!("this file is compressed (gzip): decompress it first");
    }
    r.consume(bom);
    let mut out = Vec::new();
    let format = match first {
        Some(b'<') => parse_xml(r, &mut out)?,
        Some(b'{' | b'[') => parse_json(r, &mut out)?,
        Some(b'$') => parse_nmea(r, &mut out)?,
        None => bail!("the file is empty"),
        _ => bail!("not a track file: Hyperfocal reads GPX, KML, TCX, NMEA and Google location history (JSON)"),
    };
    tidy(&mut out);
    if out.is_empty() {
        bail!("no positions with a time in this {format} file");
    }
    Ok(Track { format, fixes: out })
}

// ------------------------------------------------------------------ times

/// Milliseconds since the epoch of an ISO 8601 / RFC 3339 time
/// (`2024-05-01T10:00:00.250+02:00`); a time without a zone is taken as UTC.
pub fn parse_time(s: &str) -> Option<i64> {
    let b = s.trim().as_bytes();
    let num = |a: usize, n: usize| -> Option<i64> {
        let d = b.get(a..a + n)?;
        d.iter().all(u8::is_ascii_digit).then(|| d.iter().fold(0i64, |v, c| v * 10 + (c - b'0') as i64))
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut ms = crate::model::days_from_civil(y, mo, d) * 86_400_000;
    let mut i = 10;
    if matches!(b.get(i), Some(b'T' | b't' | b' ')) {
        let (h, mi) = (num(i + 1, 2)?, num(i + 4, 2)?);
        if b.get(i + 3) != Some(&b':') || h > 24 || mi > 59 {
            return None;
        }
        ms += (h * 3600 + mi * 60) * 1000;
        i += 6;
        if b.get(i) == Some(&b':') {
            let sec = num(i + 1, 2)?;
            if sec > 60 {
                return None;
            }
            ms += sec * 1000;
            i += 3;
            if matches!(b.get(i), Some(b'.' | b',')) {
                i += 1;
                let mut scale = 100;
                while let Some(c) = b.get(i).filter(|c| c.is_ascii_digit()) {
                    ms += (c - b'0') as i64 * scale;
                    scale /= 10;
                    i += 1;
                }
            }
        }
    }
    match b.get(i) {
        None => Some(ms),
        Some(b'Z' | b'z') if i + 1 == b.len() => Some(ms),
        Some(&sign @ (b'+' | b'-')) => {
            let h = num(i + 1, 2)?;
            let m = match b.get(i + 3) {
                Some(b':') => num(i + 4, 2)?,
                Some(_) => num(i + 3, 2)?,
                None => 0,
            };
            let off = (h * 60 + m) * 60_000;
            Some(if sign == b'+' { ms - off } else { ms + off })
        }
        _ => None,
    }
}

/// The time zone written into a capture time (`…+02:00`, `…Z`), in minutes east of UTC.
pub fn zone_of(taken: &str) -> Option<i32> {
    let b = taken.trim().as_bytes();
    let mut i = 19.min(b.len());
    if b.len() < 19 {
        return None;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    let rest = std::str::from_utf8(&b[i..]).ok()?;
    if rest.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let sign = match rest.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = rest[1..].chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 4 && digits.len() != 2 {
        return None;
    }
    let h: i32 = digits[..2].parse().ok()?;
    let m: i32 = digits.get(2..4).map_or(Some(0), |d| d.parse().ok())?;
    (h <= 14 && m < 60).then_some(sign * (h * 60 + m))
}

/// Coordinates as Google writes them: `geo:48.1351,11.5820`, `48.1351°, 11.5820°`.
fn parse_latlng(s: &str) -> Option<(f64, f64)> {
    let s = s.trim();
    let s = s.strip_prefix("geo:").unwrap_or(s);
    let s = s.split(';').next()?; // geo: URIs may carry parameters
    let mut it = s.split(',').map(|p| p.trim().trim_end_matches('°').trim());
    let lat = it.next()?.parse().ok()?;
    let lon = it.next()?.parse().ok()?;
    Some((lat, lon))
}

// ------------------------------------------------------------------ XML

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tag {
    /// GPX `trkpt`, `rtept`, `wpt`
    Pt,
    Time,
    Ele,
    /// KML `gx:Track`
    Track,
    When,
    Coord,
    Placemark,
    TimeStamp,
    TimeSpan,
    Begin,
    End,
    Point,
    LineString,
    Coordinates,
    /// TCX
    Trackpoint,
    LatDeg,
    LonDeg,
    AltM,
    Other,
}

fn tag_of(local: &[u8]) -> Tag {
    match local {
        b"trkpt" | b"rtept" | b"wpt" => Tag::Pt,
        b"time" | b"Time" => Tag::Time,
        b"ele" => Tag::Ele,
        b"Track" => Tag::Track,
        b"when" => Tag::When,
        b"coord" => Tag::Coord,
        b"Placemark" => Tag::Placemark,
        b"TimeStamp" => Tag::TimeStamp,
        b"TimeSpan" => Tag::TimeSpan,
        b"begin" => Tag::Begin,
        b"end" => Tag::End,
        b"Point" => Tag::Point,
        b"LineString" => Tag::LineString,
        b"coordinates" => Tag::Coordinates,
        b"Trackpoint" => Tag::Trackpoint,
        b"LatitudeDegrees" => Tag::LatDeg,
        b"LongitudeDegrees" => Tag::LonDeg,
        b"AltitudeMeters" => Tag::AltM,
        _ => Tag::Other,
    }
}

#[derive(Default)]
struct XmlState {
    // GPX point / TCX trackpoint
    lat: Option<f64>,
    lon: Option<f64>,
    time: Option<i64>,
    ele: Option<f32>,
    // KML gx:Track
    whens: Vec<Option<i64>>,
    coords: Vec<Option<(f64, f64, f32)>>,
    // KML Placemark
    pm_when: Option<i64>,
    pm_begin: Option<i64>,
    pm_end: Option<i64>,
    pm_point: Option<(f64, f64, f32)>,
    pm_line: Vec<(f64, f64, f32)>,
}

/// `lon,lat[,alt]` (KML `coordinates`) or `lon lat [alt]` (KML `gx:coord`).
fn kml_coord(s: &str, sep: char) -> Option<(f64, f64, f32)> {
    let mut it = s.split(sep).map(str::trim).filter(|p| !p.is_empty());
    let lon: f64 = it.next()?.parse().ok()?;
    let lat: f64 = it.next()?.parse().ok()?;
    let alt: f32 = it.next().and_then(|a| a.parse().ok()).unwrap_or(f32::NAN);
    Some((lat, lon, alt))
}

fn attr_f64(e: &BytesStart, name: &[u8]) -> Option<f64> {
    e.attributes().flatten().find(|a| a.key.local_name().as_ref() == name).and_then(|a| std::str::from_utf8(&a.value).ok()?.trim().parse().ok())
}

fn parse_xml<R: BufRead>(r: R, out: &mut Vec<Fix>) -> Result<&'static str> {
    let mut reader = quick_xml::Reader::from_reader(r);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut stack: Vec<Tag> = Vec::new();
    let mut text = String::new();
    let mut st = XmlState::default();
    let mut format: Option<&'static str> = None;
    loop {
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) => {
                // A track cut short still has its fixes up to the damage.
                if format.is_some() && !out.is_empty() {
                    break;
                }
                bail!("not a readable XML file: {e}");
            }
        };
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) if format.is_none() => {
                let is_start = matches!(ev, Event::Start(_));
                format = Some(match e.local_name().as_ref() {
                    b"gpx" => "GPX",
                    b"kml" => "KML",
                    b"TrainingCenterDatabase" => "TCX",
                    other => bail!("not a track file (an XML file whose root is <{}>)", String::from_utf8_lossy(other)),
                });
                if is_start {
                    stack.push(Tag::Other);
                }
            }
            Event::Start(e) => {
                let tag = tag_of(e.local_name().as_ref());
                open(tag, &e, &mut st);
                stack.push(tag);
                text.clear();
            }
            Event::Empty(e) => {
                let tag = tag_of(e.local_name().as_ref());
                open(tag, &e, &mut st);
                stack.push(tag);
                text.clear();
                close(&stack, &text, &mut st, out);
                stack.pop();
            }
            Event::Text(t) => {
                if let Ok(s) = t.unescape() {
                    text.push_str(&s);
                }
            }
            Event::CData(t) => text.push_str(&String::from_utf8_lossy(&t)),
            Event::End(_) => {
                close(&stack, &text, &mut st, out);
                stack.pop();
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    format.ok_or_else(|| anyhow::anyhow!("not a track file"))
}

fn open(tag: Tag, e: &BytesStart, st: &mut XmlState) {
    match tag {
        Tag::Pt => {
            st.lat = attr_f64(e, b"lat");
            st.lon = attr_f64(e, b"lon");
            st.time = None;
            st.ele = None;
        }
        Tag::Trackpoint => {
            st.lat = None;
            st.lon = None;
            st.time = None;
            st.ele = None;
        }
        Tag::Track => {
            st.whens.clear();
            st.coords.clear();
        }
        Tag::Placemark => {
            st.pm_when = None;
            st.pm_begin = None;
            st.pm_end = None;
            st.pm_point = None;
            st.pm_line.clear();
        }
        _ => {}
    }
}

/// The element on top of `stack` ends with `text` as its content.
fn close(stack: &[Tag], text: &str, st: &mut XmlState, out: &mut Vec<Fix>) {
    let Some(&tag) = stack.last() else { return };
    let parent = stack.len().checked_sub(2).map(|i| stack[i]);
    let inside = |t: Tag| stack.contains(&t);
    match tag {
        Tag::Time if parent == Some(Tag::Pt) || parent == Some(Tag::Trackpoint) => st.time = parse_time(text),
        Tag::Ele if parent == Some(Tag::Pt) => st.ele = text.trim().parse().ok(),
        Tag::AltM if parent == Some(Tag::Trackpoint) => st.ele = text.trim().parse().ok(),
        Tag::LatDeg if inside(Tag::Trackpoint) => st.lat = text.trim().parse().ok(),
        Tag::LonDeg if inside(Tag::Trackpoint) => st.lon = text.trim().parse().ok(),
        Tag::Pt | Tag::Trackpoint => {
            if let (Some(t), Some(lat), Some(lon)) = (st.time, st.lat, st.lon) {
                out.extend(fix(t, lat, lon, st.ele.unwrap_or(f32::NAN)));
            }
        }
        Tag::When if parent == Some(Tag::Track) => st.whens.push(parse_time(text)),
        Tag::When if parent == Some(Tag::TimeStamp) => st.pm_when = parse_time(text),
        Tag::Coord if parent == Some(Tag::Track) => st.coords.push(kml_coord(text, ' ')),
        Tag::Begin if parent == Some(Tag::TimeSpan) => st.pm_begin = parse_time(text),
        Tag::End if parent == Some(Tag::TimeSpan) => st.pm_end = parse_time(text),
        Tag::Coordinates if parent == Some(Tag::Point) => st.pm_point = text.split_whitespace().next().and_then(|c| kml_coord(c, ',')),
        Tag::Coordinates if parent == Some(Tag::LineString) => st.pm_line = text.split_whitespace().filter_map(|c| kml_coord(c, ',')).collect(),
        Tag::Track => {
            for (w, c) in st.whens.iter().zip(&st.coords) {
                if let (Some(t), Some((lat, lon, alt))) = (w, c) {
                    out.extend(fix(*t, *lat, *lon, *alt));
                }
            }
            st.whens.clear();
            st.coords.clear();
        }
        Tag::Placemark => placemark(st, out),
        _ => {}
    }
}

fn placemark(st: &mut XmlState, out: &mut Vec<Fix>) {
    if let Some((lat, lon, alt)) = st.pm_point {
        // A point at a moment, or a stay from `begin` to `end`.
        if let Some(t) = st.pm_when {
            out.extend(fix(t, lat, lon, alt));
        }
        stay(out, st.pm_begin, st.pm_end, lat, lon, alt);
    } else if let (Some(b), Some(e), true) = (st.pm_begin, st.pm_end, st.pm_line.len() >= 2) {
        // A path walked from `begin` to `end`: spread the time along its length.
        let line = &st.pm_line;
        let mut acc = vec![0.0];
        for w in line.windows(2) {
            acc.push(acc.last().unwrap() + distance_m(w[0].0, w[0].1, w[1].0, w[1].1));
        }
        let total = *acc.last().unwrap();
        for (i, &(lat, lon, alt)) in line.iter().enumerate() {
            let f = if total > 0.0 { acc[i] / total } else { i as f64 / (line.len() - 1) as f64 };
            out.extend(fix(b + ((e - b) as f64 * f).round() as i64, lat, lon, alt));
        }
    }
}

/// Great-circle distance in metres.
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6_371_008.8 * a.sqrt().min(1.0).asin()
}

// ------------------------------------------------------------------ NMEA

fn parse_nmea<R: BufRead>(r: R, out: &mut Vec<Fix>) -> Result<&'static str> {
    // Altitude comes from GGA sentences (no date), matched to RMC by time of day.
    let mut gga: Option<(String, f32)> = None;
    for line in r.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        let body = line.split('*').next().unwrap_or("");
        let f: Vec<&str> = body.split(',').collect();
        let Some(kind) = f.first().filter(|k| k.starts_with('$') && k.len() == 6) else { continue };
        let coord = |v: &str, hemi: &str, deg_digits: usize| -> Option<f64> {
            if v.len() <= deg_digits {
                return None;
            }
            let d: f64 = v[..deg_digits].parse().ok()?;
            let m: f64 = v[deg_digits..].parse().ok()?;
            let x = d + m / 60.0;
            Some(if hemi == "S" || hemi == "W" { -x } else { x })
        };
        if kind.ends_with("GGA") && f.len() > 9 {
            gga = f[9].parse().ok().map(|a| (f[1].to_string(), a));
        } else if kind.ends_with("RMC") && f.len() > 9 && f[2] == "A" {
            let (tm, date) = (f[1], f[9]);
            if tm.len() < 6 || date.len() != 6 {
                continue;
            }
            let p = |s: &str| s.parse::<i64>().ok();
            let (Some(h), Some(mi), Some(s), Some(dd), Some(mm), Some(yy)) = (p(&tm[0..2]), p(&tm[2..4]), p(&tm[4..6]), p(&date[0..2]), p(&date[2..4]), p(&date[4..6])) else { continue };
            let frac = tm.get(6..).and_then(|x| format!("0{x}").parse::<f64>().ok()).unwrap_or(0.0);
            let year = if yy < 80 { 2000 + yy } else { 1900 + yy };
            if !(1..=12).contains(&mm) || !(1..=31).contains(&dd) {
                continue;
            }
            let t = (crate::model::days_from_civil(year, mm, dd) * 86400 + h * 3600 + mi * 60 + s) * 1000 + (frac * 1000.0).round() as i64;
            let ele = gga.as_ref().filter(|(gt, _)| gt.get(..6) == tm.get(..6)).map_or(f32::NAN, |g| g.1);
            if let (Some(lat), Some(lon)) = (coord(f[3], f[4], 2), coord(f[5], f[6], 3)) {
                out.extend(fix(t, lat, lon, ele));
            }
        }
    }
    Ok("NMEA")
}

// ------------------------------------------------------------------ Google JSON

/// A JSON number that may be written as a string (`"9"`), as the iPhone export does.
#[derive(Deserialize)]
#[serde(untagged)]
enum Num {
    N(f64),
    S(String),
}

impl Num {
    fn get(&self) -> Option<f64> {
        match self {
            Num::N(v) => Some(*v),
            Num::S(s) => s.trim().parse().ok(),
        }
    }
}

/// A place as `"geo:lat,lng"` (iPhone) or `{"latLng": "lat°, lng°"}` (Android).
#[derive(Deserialize)]
#[serde(untagged)]
enum Place {
    S(String),
    O {
        #[serde(rename = "latLng", alias = "LatLng")]
        lat_lng: Option<String>,
    },
}

impl Place {
    fn get(&self) -> Option<(f64, f64)> {
        match self {
            Place::S(s) => parse_latlng(s),
            Place::O { lat_lng } => parse_latlng(lat_lng.as_deref()?),
        }
    }
}

fn e7(v: Option<i64>) -> Option<f64> {
    v.map(|x| x as f64 / 1e7)
}

/// `timestamp` (RFC 3339) or the older `timestampMs` (epoch milliseconds as a string).
fn stamp(iso: &Option<String>, ms: &Option<Num>) -> Option<i64> {
    iso.as_deref().and_then(parse_time).or_else(|| ms.as_ref()?.get().map(|v| v as i64))
}

/// Takeout `Records.json`: `{"locations": [...]}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordLoc {
    latitude_e7: Option<i64>,
    longitude_e7: Option<i64>,
    timestamp: Option<String>,
    timestamp_ms: Option<Num>,
    altitude: Option<f64>,
    accuracy: Option<f64>,
}

/// Takeout semantic location history (`2022_JANUARY.json`): `{"timelineObjects": [...]}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimelineObject {
    place_visit: Option<PlaceVisit>,
    activity_segment: Option<ActivitySegment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct E7Loc {
    latitude_e7: Option<i64>,
    longitude_e7: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Duration {
    start_timestamp: Option<String>,
    end_timestamp: Option<String>,
    start_timestamp_ms: Option<Num>,
    end_timestamp_ms: Option<Num>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaceVisit {
    location: Option<E7Loc>,
    duration: Option<Duration>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivitySegment {
    start_location: Option<E7Loc>,
    end_location: Option<E7Loc>,
    duration: Option<Duration>,
    simplified_raw_path: Option<RawPath>,
}

#[derive(Deserialize)]
struct RawPath {
    #[serde(default)]
    points: Vec<RawPathPoint>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPathPoint {
    lat_e7: Option<i64>,
    lng_e7: Option<i64>,
    timestamp: Option<String>,
    timestamp_ms: Option<Num>,
}

/// The phone's export: `semanticSegments` (Android `Timeline.json`), or a bare array of
/// them (iPhone).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Segment {
    start_time: Option<String>,
    end_time: Option<String>,
    visit: Option<Visit>,
    activity: Option<Activity>,
    timeline_path: Option<Vec<PathPoint>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Visit {
    top_candidate: Option<Candidate>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    place_location: Option<Place>,
}

#[derive(Deserialize)]
struct Activity {
    start: Option<Place>,
    end: Option<Place>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathPoint {
    point: Option<String>,
    time: Option<String>,
    duration_minutes_offset_from_start_time: Option<Num>,
}

/// Android `rawSignals`: `{"position": {...}}` among Wi-Fi scans and activity records.
#[derive(Deserialize)]
struct RawSignal {
    position: Option<Position>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Position {
    #[serde(rename = "LatLng", alias = "latLng")]
    lat_lng: Option<String>,
    timestamp: Option<String>,
    altitude_meters: Option<f64>,
    accuracy_meters: Option<f64>,
}

fn segment(s: Segment, out: &mut Vec<Fix>) {
    let start = s.start_time.as_deref().and_then(parse_time);
    let end = s.end_time.as_deref().and_then(parse_time);
    if let Some((lat, lon)) = s.visit.as_ref().and_then(|v| v.top_candidate.as_ref()?.place_location.as_ref()?.get()) {
        stay(out, start, end, lat, lon, f32::NAN);
    }
    let mut put = |t: Option<i64>, p: Option<(f64, f64)>| {
        if let (Some(t), Some((lat, lon))) = (t, p) {
            out.extend(fix(t, lat, lon, f32::NAN));
        }
    };
    if let Some(a) = &s.activity {
        put(start, a.start.as_ref().and_then(Place::get));
        put(end, a.end.as_ref().and_then(Place::get));
    }
    for pt in s.timeline_path.iter().flatten() {
        let t = pt.time.as_deref().and_then(parse_time).or_else(|| {
            let mins = pt.duration_minutes_offset_from_start_time.as_ref()?.get()?;
            Some(start? + (mins * 60_000.0).round() as i64)
        });
        put(t, pt.point.as_deref().and_then(parse_latlng));
    }
}

/// Calls `f` for each element of a JSON array, without collecting them.
struct Each<T, F>(F, PhantomData<T>);

impl<'de, T: Deserialize<'de>, F: FnMut(T)> DeserializeSeed<'de> for Each<T, F> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de, T: Deserialize<'de>, F: FnMut(T)> Visitor<'de> for Each<T, F> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list")
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut s: A) -> Result<(), A::Error> {
        while let Some(x) = s.next_element::<T>()? {
            (self.0)(x);
        }
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
}

fn each<T, F: FnMut(T)>(f: F) -> Each<T, F> {
    Each(f, PhantomData)
}

/// The top level of any of Google's formats, told apart by its keys.
struct Top<'a> {
    out: &'a mut Vec<Fix>,
    format: Option<&'static str>,
}

impl<'de> Visitor<'de> for &mut Top<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a Google location history export")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<(), A::Error> {
        while let Some(k) = m.next_key::<String>()? {
            let out = &mut *self.out;
            match k.as_str() {
                "locations" => {
                    self.format = Some("Google location history (Records.json)");
                    m.next_value_seed(each(|r: RecordLoc| {
                        if r.accuracy.is_some_and(|a| a > MAX_ACCURACY_M) {
                            return;
                        }
                        if let (Some(t), Some(lat), Some(lon)) = (stamp(&r.timestamp, &r.timestamp_ms), e7(r.latitude_e7), e7(r.longitude_e7)) {
                            out.extend(fix(t, lat, lon, r.altitude.map_or(f32::NAN, |a| a as f32)));
                        }
                    }))?;
                }
                "timelineObjects" => {
                    self.format = Some("Google location history (semantic)");
                    m.next_value_seed(each(|o: TimelineObject| semantic(o, out)))?;
                }
                "semanticSegments" => {
                    self.format = Some("Google Timeline (phone export)");
                    m.next_value_seed(each(|s: Segment| segment(s, out)))?;
                }
                "rawSignals" => {
                    self.format = Some("Google Timeline (phone export)");
                    m.next_value_seed(each(|r: RawSignal| {
                        let Some(p) = r.position else { return };
                        if p.accuracy_meters.is_some_and(|a| a > MAX_ACCURACY_M) {
                            return;
                        }
                        if let (Some(t), Some((lat, lon))) = (p.timestamp.as_deref().and_then(parse_time), p.lat_lng.as_deref().and_then(parse_latlng)) {
                            out.extend(fix(t, lat, lon, p.altitude_meters.map_or(f32::NAN, |a| a as f32)));
                        }
                    }))?;
                }
                _ => {
                    m.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<(), A::Error> {
        self.format = Some("Google Timeline (iPhone export)");
        while let Some(seg) = s.next_element::<Segment>()? {
            segment(seg, &mut *self.out);
        }
        Ok(())
    }
}

fn semantic(o: TimelineObject, out: &mut Vec<Fix>) {
    let times = |d: &Option<Duration>| -> (Option<i64>, Option<i64>) {
        d.as_ref().map_or((None, None), |d| (stamp(&d.start_timestamp, &d.start_timestamp_ms), stamp(&d.end_timestamp, &d.end_timestamp_ms)))
    };
    let at = |l: &Option<E7Loc>| l.as_ref().and_then(|l| Some((e7(l.latitude_e7)?, e7(l.longitude_e7)?)));
    if let Some(v) = &o.place_visit
        && let Some((lat, lon)) = at(&v.location)
    {
        let (b, e) = times(&v.duration);
        stay(out, b, e, lat, lon, f32::NAN);
    }
    let mut put = |t: Option<i64>, p: Option<(f64, f64)>| {
        if let (Some(t), Some((lat, lon))) = (t, p) {
            out.extend(fix(t, lat, lon, f32::NAN));
        }
    };
    if let Some(a) = &o.activity_segment {
        let (b, e) = times(&a.duration);
        put(b, at(&a.start_location));
        put(e, at(&a.end_location));
        for p in a.simplified_raw_path.iter().flat_map(|r| &r.points) {
            put(stamp(&p.timestamp, &p.timestamp_ms), e7(p.lat_e7).zip(e7(p.lng_e7)));
        }
    }
}

fn parse_json<R: BufRead>(r: R, out: &mut Vec<Fix>) -> Result<&'static str> {
    let mut top = Top { out, format: None };
    let mut de = serde_json::Deserializer::from_reader(r);
    if let Err(e) = de::Deserializer::deserialize_any(&mut de, &mut top) {
        // A file cut short (an interrupted download) still yields what came before.
        if !(e.is_eof() && !top.out.is_empty()) {
            bail!("not a Google location history file: {e}");
        }
    }
    top.format.ok_or_else(|| anyhow::anyhow!("not a Google location history file (no locations, timelineObjects or semanticSegments)"))
}

// ------------------------------------------------------------------ matching

/// Past a gap, a photo takes the nearer fix only if it is this close in time (ms) and
/// the gap limit allows.
pub const MAX_NEAREST_MS: i64 = 10 * 60_000;
/// The longest stay at one place that is taken at its word.
pub const MAX_STAY_MS: i64 = 48 * 3_600_000;

/// The position at `t` (ms, UTC): interpolated between the fixes around it when they
/// are at most `max_gap` ms apart (or a stay lasts until the next), else the nearer one
/// when it is close enough in time.
pub fn locate(fixes: &[Fix], t: i64, max_gap: i64) -> Option<Fix> {
    let i = fixes.partition_point(|f| f.t < t);
    if let Some(f) = fixes.get(i).filter(|f| f.t == t) {
        return Some(*f);
    }
    let prev = i.checked_sub(1).map(|j| fixes[j]);
    let next = fixes.get(i).copied();
    if let (Some(a), Some(b)) = (prev, next)
        && (b.t - a.t <= max_gap || (a.stay && b.t - a.t <= MAX_STAY_MS))
    {
        let f = (t - a.t) as f64 / (b.t - a.t) as f64;
        let mut dl = b.lon - a.lon;
        if dl > 180.0 {
            dl -= 360.0;
        } else if dl < -180.0 {
            dl += 360.0;
        }
        let mut lon = a.lon + f * dl;
        if lon > 180.0 {
            lon -= 360.0;
        } else if lon < -180.0 {
            lon += 360.0;
        }
        let ele = if a.ele.is_finite() && b.ele.is_finite() { a.ele + (b.ele - a.ele) * f as f32 } else { f32::NAN };
        return Some(Fix { t, lat: a.lat + f * (b.lat - a.lat), lon, ele, stay: false });
    }
    let reach = max_gap.min(MAX_NEAREST_MS);
    [prev, next].into_iter().flatten().filter(|f| (f.t - t).abs() <= reach).min_by_key(|f| (f.t - t).abs()).map(|f| Fix { t, stay: false, ..f })
}

/// Fixes merged from several tracks.
pub fn merge(tracks: &[&[Fix]]) -> Vec<Fix> {
    let mut all: Vec<Fix> = tracks.iter().flat_map(|t| t.iter().copied()).collect();
    tidy(&mut all);
    all
}

/// The camera's time zone (minutes east of UTC) under which the most photos fall on
/// the track. `local` are camera clock times read as UTC (seconds). Offsets that tie
/// are broken towards `prefer` (the browser's zone), then whole hours near the middle
/// of the tie. Returns (best, lowest and highest tied offset, matches per offset).
pub fn detect_zone(fixes: &[Fix], local: &[i64], max_gap: i64, prefer: i32) -> (i32, (i32, i32), Vec<(i32, usize)>) {
    // A sample keeps this quick on a big library: 105 offsets × 2000 photos.
    let step = local.len().div_ceil(2000).max(1);
    let sample: Vec<i64> = local.iter().step_by(step).copied().collect();
    let counts: Vec<(i32, usize)> = (-720..=840)
        .step_by(15)
        .map(|o: i32| (o, sample.iter().filter(|&&s| locate(fixes, (s - o as i64 * 60) * 1000, max_gap).is_some()).count()))
        .collect();
    let best = counts.iter().map(|c| c.1).max().unwrap_or(0);
    if best == 0 {
        return (prefer, (prefer, prefer), counts);
    }
    let tied: Vec<i32> = counts.iter().filter(|c| c.1 == best).map(|c| c.0).collect();
    let plateau = (tied[0], *tied.last().unwrap());
    if tied.contains(&prefer) {
        return (prefer, plateau, counts);
    }
    let mid = tied[tied.len() / 2];
    let pick = [60, 30, 15]
        .iter()
        .find_map(|unit| tied.iter().filter(|o| *o % unit == 0).min_by_key(|o| ((*o - mid).abs(), **o)).copied())
        .unwrap_or(mid);
    (pick, plateau, counts)
}

/// At most `max` fixes for drawing, split where the track has a gap longer than
/// `gap` ms (so a map does not draw straight lines across them).
pub fn outline(fixes: &[Fix], max: usize, gap: i64) -> Vec<Vec<[f64; 2]>> {
    let step = fixes.len().div_ceil(max.max(1)).max(1);
    let pt = |f: &Fix| [(f.lat * 1e6).round() / 1e6, (f.lon * 1e6).round() / 1e6];
    let mut lines: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut cur: Vec<[f64; 2]> = Vec::new();
    for (i, f) in fixes.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| &fixes[j]);
        let brk = prev.is_some_and(|p| f.t - p.t > gap && !p.stay);
        if brk {
            // End the line at its last fix, start the next at its first.
            let last = pt(prev.unwrap());
            if cur.last() != Some(&last) {
                cur.push(last);
            }
            lines.push(std::mem::take(&mut cur));
        }
        if brk || i % step == 0 || i + 1 == fixes.len() {
            cur.push(pt(f));
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(s: &str) -> Result<Track> {
        parse(std::io::BufReader::new(s.as_bytes()))
    }

    const T0: i64 = 1_714_557_600_000; // 2024-05-01T10:00:00Z

    #[test]
    fn times() {
        assert_eq!(parse_time("2024-05-01T10:00:00Z"), Some(T0));
        assert_eq!(parse_time("2024-05-01T10:00:00"), Some(T0));
        assert_eq!(parse_time("2024-05-01T12:00:00+02:00"), Some(T0));
        assert_eq!(parse_time("2024-05-01T05:30:00-0430"), Some(T0));
        assert_eq!(parse_time("2024-05-01T10:00:00.250Z"), Some(T0 + 250));
        assert_eq!(parse_time("2024-05-01T10:00:00.1234567+00:00"), Some(T0 + 123));
        assert_eq!(parse_time("2024-05-01 10:00:00"), Some(T0));
        assert_eq!(parse_time("2024-05-01T10:00Z"), Some(T0));
        assert_eq!(parse_time("2024-05-01"), Some(T0 - 10 * 3_600_000));
        for bad in ["", "2024", "2024-13-01T00:00:00Z", "2024-05-01T25:00:00Z", "2024-05-01T10:00:00Q", "yesterday"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
        assert_eq!(zone_of("2024-05-01T10:00:00+02:00"), Some(120));
        assert_eq!(zone_of("2024-05-01T10:00:00.12-03:30"), Some(-210));
        assert_eq!(zone_of("2024-05-01T10:00:00Z"), Some(0));
        assert_eq!(zone_of("2024-05-01T10:00:00"), None);
        assert_eq!(zone_of("2024-05-01T10:00:00+0530"), Some(330));
        assert_eq!(zone_of("2024-05-01"), None);
        assert_eq!(parse_latlng("geo:51.512071,-0.127062"), Some((51.512071, -0.127062)));
        assert_eq!(parse_latlng("41.4537693°, 1.9644003°"), Some((41.4537693, 1.9644003)));
        assert_eq!(parse_latlng("nowhere"), None);
    }

    #[test]
    fn gpx() {
        let t = read(
            r#"<?xml version="1.0"?>
<gpx version="1.1" creator="x" xmlns="http://www.topografix.com/GPX/1/1">
 <metadata><time>2020-01-01T00:00:00Z</time></metadata>
 <trk><name>Hike</name><trkseg>
  <trkpt lat="38.5" lon="-120.2"><ele>2376.5</ele><time>2024-05-01T10:00:00Z</time></trkpt>
  <trkpt lat="38.6" lon="-120.3"><time>2024-05-01T10:01:00Z</time></trkpt>
  <trkpt lat="38.7" lon="-120.4"/>
  <trkpt lat="38.6" lon="-120.3"><time>2024-05-01T10:01:00Z</time></trkpt>
 </trkseg></trk>
 <wpt lat="1" lon="2"><time>2024-05-01T09:00:00Z</time></wpt>
</gpx>"#,
        )
        .unwrap();
        assert_eq!(t.format, "GPX");
        assert_eq!(t.fixes.len(), 3, "{:?}", t.fixes);
        assert_eq!(t.fixes[0].t, T0 - 3_600_000);
        assert_eq!((t.fixes[1].lat, t.fixes[1].lon, t.fixes[1].ele), (38.5, -120.2, 2376.5));
        assert!(t.fixes[2].ele.is_nan());
    }

    #[test]
    fn kml_and_tcx() {
        let t = read(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2" xmlns:gx="http://www.google.com/kml/ext/2.2"><Document>
 <Placemark><gx:Track>
  <when>2024-05-01T10:00:00Z</when><when>2024-05-01T10:00:10Z</when>
  <gx:coord>-122.2 37.4 150</gx:coord><gx:coord>-122.3 37.5 151</gx:coord>
 </gx:Track></Placemark>
 <Placemark><TimeStamp><when>2024-05-01T11:00:00Z</when></TimeStamp><Point><coordinates>2.35,48.85,30</coordinates></Point></Placemark>
 <Placemark><TimeSpan><begin>2024-05-01T12:00:00Z</begin><end>2024-05-01T13:00:00Z</end></TimeSpan><Point><coordinates>2.29,48.86</coordinates></Point></Placemark>
 <Placemark><TimeSpan><begin>2024-05-01T14:00:00Z</begin><end>2024-05-01T14:10:00Z</end></TimeSpan>
  <LineString><coordinates>10,50 10,50.01 10,50.03</coordinates></LineString></Placemark>
</Document></kml>"#,
        )
        .unwrap();
        assert_eq!(t.format, "KML");
        let ts: Vec<i64> = t.fixes.iter().map(|f| (f.t - T0) / 1000).collect();
        assert_eq!(ts, vec![0, 10, 3600, 7200, 10800, 14400, 14400 + 200, 15000]);
        assert_eq!((t.fixes[0].lat, t.fixes[0].lon, t.fixes[0].ele), (37.4, -122.2, 150.0));
        assert_eq!((t.fixes[2].lat, t.fixes[2].lon), (48.85, 2.35));
        // The stay from 12:00 to 13:00 holds the place for the whole hour.
        assert!(t.fixes[3].stay && !t.fixes[4].stay && !t.fixes[2].stay);
        assert_eq!(locate(&t.fixes, T0 + 9000 * 1000, 60_000).map(|f| f.lon), Some(2.29));

        let t = read(
            r#"<TrainingCenterDatabase xmlns="http://www.garmin.com/xmlschemas/TrainingCenterDatabase/v2"><Activities><Activity><Lap><Track>
 <Trackpoint><Time>2024-05-01T10:00:00.000Z</Time><Position><LatitudeDegrees>47.1</LatitudeDegrees><LongitudeDegrees>8.5</LongitudeDegrees></Position><AltitudeMeters>410.2</AltitudeMeters></Trackpoint>
 <Trackpoint><Time>2024-05-01T10:00:05.000Z</Time></Trackpoint>
</Track></Lap></Activity></Activities></TrainingCenterDatabase>"#,
        )
        .unwrap();
        assert_eq!(t.format, "TCX");
        assert_eq!(t.fixes, vec![Fix { t: T0, lat: 47.1, lon: 8.5, ele: 410.2, stay: false }]);
    }

    #[test]
    fn nmea() {
        let t = read(
            "$GPGGA,100000.00,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\n\
             $GPRMC,100000.00,A,4807.038,N,01131.000,E,022.4,084.4,010524,003.1,W*6A\n\
             $GPRMC,100001,V,4807.038,N,01131.000,E,022.4,084.4,010524,003.1,W*6A\n\
             $GNRMC,100002.50,A,3351.000,S,15112.000,W,0,0,010524,,*00\n",
        )
        .unwrap();
        assert_eq!(t.format, "NMEA");
        assert_eq!(t.fixes.len(), 2);
        assert_eq!(t.fixes[0].t, T0);
        assert!((t.fixes[0].lat - 48.1173).abs() < 1e-9 && (t.fixes[0].lon - 11.516_666_666).abs() < 1e-6);
        assert_eq!(t.fixes[0].ele, 545.4);
        assert_eq!(t.fixes[1].t, T0 + 2500);
        assert!(t.fixes[1].lat < 0.0 && t.fixes[1].lon < 0.0 && t.fixes[1].ele.is_nan());
    }

    #[test]
    fn google_records_and_semantic() {
        let t = read(
            r#"{"locations": [
              {"latitudeE7": 414216106, "longitudeE7": 21684775, "accuracy": 20, "timestamp": "2024-05-01T10:00:00.000Z", "activity": [{"x": 1}]},
              {"latitudeE7": 414216106, "longitudeE7": 21684775, "accuracy": 18000, "timestamp": "2024-05-01T10:01:00Z"},
              {"latitudeE7": 414216000, "longitudeE7": 21684000, "timestampMs": "1714557720000", "altitude": 80},
              {"latitudeE7": 1, "timestamp": "2024-05-01T10:03:00Z"}
            ], "other": {"a": [1, 2]}}"#,
        )
        .unwrap();
        assert_eq!(t.format, "Google location history (Records.json)");
        assert_eq!(t.fixes.len(), 2);
        assert_eq!(t.fixes[1].t, T0 + 120_000);
        assert_eq!(t.fixes[1].ele, 80.0);
        assert!((t.fixes[0].lat - 41.4216106).abs() < 1e-9);

        let t = read(
            r#"{"timelineObjects": [
              {"placeVisit": {"location": {"latitudeE7": 485000000, "longitudeE7": 23000000, "name": "Café"},
                "duration": {"startTimestamp": "2024-05-01T10:00:00Z", "endTimestamp": "2024-05-01T11:00:00Z"}}},
              {"activitySegment": {"startLocation": {"latitudeE7": 485000000, "longitudeE7": 23000000},
                "endLocation": {"latitudeE7": 486000000, "longitudeE7": 23100000},
                "duration": {"startTimestampMs": "1714561200000", "endTimestampMs": "1714563000000"},
                "simplifiedRawPath": {"points": [{"latE7": 485500000, "lngE7": 23050000, "timestamp": "2024-05-01T11:15:00Z"}]},
                "waypointPath": {"waypoints": [{"latE7": 1, "lngE7": 2}]}}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(t.format, "Google location history (semantic)");
        let ts: Vec<i64> = t.fixes.iter().map(|f| (f.t - T0) / 60_000).collect();
        assert_eq!(ts, vec![0, 60, 75, 90], "the visit's end and the activity's start are one fix");
        assert!(t.fixes[0].stay && !t.fixes[1].stay);
    }

    #[test]
    fn google_phone_exports() {
        let android = r#"{"semanticSegments": [
            {"startTime": "2024-05-01T12:00:00.000+02:00", "endTime": "2024-05-01T13:00:00.000+02:00",
             "visit": {"hierarchyLevel": 0, "probability": 0.9, "topCandidate": {"placeId": "x", "placeLocation": {"latLng": "48.1351°, 11.5820°"}}}},
            {"startTime": "2024-05-01T13:00:00.000+02:00", "endTime": "2024-05-01T13:30:00.000+02:00",
             "activity": {"start": {"latLng": "48.1351°, 11.5820°"}, "end": {"latLng": "48.2°, 11.6°"}, "distanceMeters": 1234.5, "topCandidate": {"type": "WALKING"}}},
            {"startTime": "2024-05-01T13:00:00.000+02:00", "endTime": "2024-05-01T15:00:00.000+02:00",
             "timelinePath": [{"point": "48.15°, 11.59°", "time": "2024-05-01T13:10:00.000+02:00"}]}
          ],
          "rawSignals": [
            {"position": {"LatLng": "48.3°, 11.7°", "accuracyMeters": 13, "altitudeMeters": 520.5, "source": "GPS", "timestamp": "2024-05-01T14:00:00.000+02:00"}},
            {"wifiScan": {"deliveryTime": "2024-05-01T14:00:00.000+02:00", "devicesRecords": []}},
            {"position": {"LatLng": "48.4°, 11.8°", "accuracyMeters": 5000, "timestamp": "2024-05-01T14:05:00.000+02:00"}}
          ],
          "userLocationProfile": {"frequentPlaces": []}}"#;
        let t = read(android).unwrap();
        assert_eq!(t.format, "Google Timeline (phone export)");
        let ts: Vec<i64> = t.fixes.iter().map(|f| (f.t - T0) / 60_000).collect();
        assert_eq!(ts, vec![0, 60, 70, 90, 120]);
        assert_eq!((t.fixes[0].lat, t.fixes[0].lon), (48.1351, 11.582));
        assert_eq!(t.fixes[4].ele, 520.5);
        assert!(t.fixes[0].stay, "a visit is a stay");

        let iphone = r#"[
          {"endTime": "2024-05-01T13:00:00.000+02:00", "startTime": "2024-05-01T12:00:00.000+02:00",
           "visit": {"hierarchyLevel": "0", "topCandidate": {"probability": "0.9", "semanticType": "Unknown", "placeID": "x", "placeLocation": "geo:48.135100,11.582000"}, "probability": "0.8"}},
          {"endTime": "2024-05-01T13:30:00.000+02:00", "startTime": "2024-05-01T13:00:00.000+02:00",
           "activity": {"start": "geo:48.1351,11.5820", "end": "geo:48.2,11.6", "topCandidate": {"type": "walking", "probability": "0.9"}, "distanceMeters": "1234.0"}},
          {"endTime": "2024-05-01T15:00:00.000+02:00", "startTime": "2024-05-01T13:00:00.000+02:00",
           "timelinePath": [{"point": "geo:48.15,11.59", "durationMinutesOffsetFromStartTime": "10"}, {"point": "geo:48.16,11.60", "durationMinutesOffsetFromStartTime": 20}]}
        ]"#;
        let t = read(iphone).unwrap();
        assert_eq!(t.format, "Google Timeline (iPhone export)");
        let ts: Vec<i64> = t.fixes.iter().map(|f| (f.t - T0) / 60_000).collect();
        assert_eq!(ts, vec![0, 60, 70, 80, 90]);
    }

    #[test]
    fn rejects_other_files() {
        for (data, msg) in [
            ("", "empty"),
            ("PK\x03\x04rest", "zip"),
            ("hello", "not a track file"),
            ("<html><body/></html>", "root is <html>"),
            (r#"{"type": "FeatureCollection"}"#, "not a Google"),
            (r#"{"locations": []}"#, "no positions"),
            (r#"<gpx><trk><trkseg><trkpt lat="1" lon="2"/></trkseg></trk></gpx>"#, "no positions"),
        ] {
            let e = read(data).unwrap_err().to_string();
            assert!(e.contains(msg), "{data:?}: {e}");
        }
        let gz = parse(&b"\x1f\x8b\x08\x00rest"[..]).unwrap_err().to_string();
        assert!(gz.contains("compressed"), "{gz}");
        // Cut short: what came before the break still counts.
        let t = read(r#"{"locations": [{"latitudeE7": 414216106, "longitudeE7": 21684775, "timestamp": "2024-05-01T10:00:00Z"}, {"latit"#).unwrap();
        assert_eq!(t.fixes.len(), 1);
        let t = read(r#"<gpx><trk><trkseg><trkpt lat="1" lon="2"><time>2024-05-01T10:00:00Z</time></trkpt><trkpt lat="1" lo"#).unwrap();
        assert_eq!(t.fixes.len(), 1);
    }

    fn f(t: i64, lat: f64, lon: f64) -> Fix {
        Fix { t: T0 + t * 1000, lat, lon, ele: f32::NAN, stay: false }
    }

    #[test]
    fn locating() {
        let track = [f(0, 10.0, 20.0), f(60, 11.0, 22.0), f(4000, 12.0, 24.0)];
        let gap = 30 * 60_000;
        let at = |s: i64| locate(&track, T0 + s * 1000, gap).map(|x| (x.lat, x.lon));
        assert_eq!(at(0), Some((10.0, 20.0)));
        assert_eq!(at(30), Some((10.5, 21.0)));
        // A gap longer than allowed: the nearer fix, if within ten minutes.
        assert_eq!(at(60 + 300), Some((11.0, 22.0)));
        assert_eq!(at(4000 - 599), Some((12.0, 24.0)));
        assert_eq!(at(2000), None);
        // Before and after the track.
        assert_eq!(at(-600), Some((10.0, 20.0)));
        assert_eq!(at(-601), None);
        assert_eq!(at(4000 + 601), None);
        // A tighter limit also limits the reach of the nearest fix.
        assert_eq!(locate(&track, T0 + 90 * 1000, 60_000).map(|x| x.lat), Some(11.0));
        assert_eq!(locate(&track, T0 + 200 * 1000, 60_000), None);
        assert_eq!(locate(&[], T0, gap), None);
        // A stay holds across any gap (up to two days); a plain gap does not.
        let visit = [Fix { stay: true, ..f(0, 5.0, 6.0) }, f(3 * 3600, 5.0, 6.0), f(6 * 3600, 5.5, 6.0)];
        assert_eq!(locate(&visit, T0 + 5400 * 1000, gap).map(|x| x.lat), Some(5.0));
        assert_eq!(locate(&visit, T0 + 4 * 3600 * 1000, gap), None);
        // Stays survive merging with a fix at the same moment.
        let merged = merge(&[&[f(0, 1.0, 1.0)], &visit]);
        assert!(merged[0].stay && merged.len() == 3);
        // Across the antimeridian.
        let dateline = [f(0, 0.0, 179.0), f(100, 0.0, -179.0)];
        let p = locate(&dateline, T0 + 75_000, gap).unwrap();
        assert!((p.lon - (-179.5)).abs() < 1e-9, "{}", p.lon);
        let mid = locate(&dateline, T0 + 50_000, gap).unwrap();
        assert!((mid.lon.abs() - 180.0).abs() < 1e-9, "{}", mid.lon);
    }

    #[test]
    fn zones() {
        // A 3-hour walk; photos at 10:30, 11:00, 12:00 UTC taken with a camera on UTC+2.
        let track: Vec<Fix> = (0..=180).map(|m| f(m * 60, 45.0 + m as f64 * 0.001, 7.0)).collect();
        let local: Vec<i64> = [1800, 3600, 7200].iter().map(|s| (T0 / 1000 + s) + 2 * 3600).collect();
        let gap = 5 * 60_000;
        let (best, (lo, hi), counts) = detect_zone(&track, &local, gap, 60);
        assert_eq!(counts.len(), 105);
        // Offsets from +1:00 (photos shifted an hour later, still on the track) to +2:30 match all three.
        assert_eq!((lo, hi), (60, 150));
        assert_eq!(best, 60, "the browser's zone wins a tie");
        let (best, _, _) = detect_zone(&track, &local, gap, -300);
        assert_eq!(best, 120, "else a whole hour near the middle");
        // Nothing matches: the preference stands.
        let (best, _, _) = detect_zone(&track, &[0], gap, 330);
        assert_eq!(best, 330);
    }

    #[test]
    fn outlines() {
        let track: Vec<Fix> = (0..1000).map(|i| f(i * 10, 1.0, i as f64 * 0.001)).chain((0..10).map(|i| f(100_000 + i, 2.0, 2.0))).collect();
        let lines = outline(&track, 100, 30 * 60_000);
        assert_eq!(lines.len(), 2, "split at the gap");
        assert!(lines.iter().map(Vec::len).sum::<usize>() <= 110);
        assert_eq!(*lines[0].last().unwrap(), [1.0, 0.999]);
    }
}
