//! LibRaw (child process side): metadata, embedded previews and sensor data for RAW
//! files FlashRaw cannot read.
//!
//! For ordinary Bayer sensors only the undeveloped sensor data leaves here, so the
//! server develops it with the same code as FlashRaw output (same clipping
//! detection, histogram and tone). Other sensors (X-Trans, Foveon, 4-colour, Fuji
//! SuperCCD, Phase One) are developed by LibRaw itself at half size, in linear sRGB.

#[allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code, unnecessary_transmutes, clippy::all)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/libraw.rs"));
}

use super::{BayerInfo, LibrawMeta, RawInfo, RgbInfo, frames, helper};
use anyhow::{Result, anyhow, bail};
use std::ffi::{CStr, CString, OsString};
use std::io::Write;
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub fn version() -> String {
    // SAFETY: returns a pointer to a static string.
    unsafe { CStr::from_ptr(sys::libraw_version()) }.to_string_lossy().into_owned()
}

/// An open LibRaw handle.
struct Raw(*mut sys::libraw_data_t);

impl Raw {
    fn open(path: &Path) -> Result<Raw> {
        // SAFETY: plain allocation; checked for null.
        let p = unsafe { sys::libraw_init(0) };
        if p.is_null() {
            bail!("LibRaw could not initialise");
        }
        let raw = Raw(p);
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| anyhow!("the file name contains a NUL byte"))?;
        // SAFETY: valid handle and NUL-terminated path.
        raw.check(unsafe { sys::libraw_open_file(p, c.as_ptr()) })?;
        Ok(raw)
    }

    fn check(&self, rc: c_int) -> Result<()> {
        if rc == 0 {
            return Ok(());
        }
        // SAFETY: returns a pointer to a static string for any code.
        let msg = unsafe { CStr::from_ptr(sys::libraw_strerror(rc)) }.to_string_lossy().into_owned();
        bail!("LibRaw: {msg}")
    }

    fn d(&self) -> &sys::libraw_data_t {
        // SAFETY: non-null handle owned by self.
        unsafe { &*self.0 }
    }

    fn d_mut(&mut self) -> &mut sys::libraw_data_t {
        // SAFETY: as above; exclusive through &mut self.
        unsafe { &mut *self.0 }
    }

    /// CFA colour (0 R, 1 G, 2 B, 3 second G) at a position of the visible area.
    fn color(&self, row: usize, col: usize) -> c_int {
        // SAFETY: valid handle; LibRaw handles any coordinates.
        unsafe { sys::libraw_COLOR(self.0, row as c_int, col as c_int) }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // SAFETY: frees everything LibRaw allocated for this handle, once.
        unsafe { sys::libraw_close(self.0) }
    }
}

/// An image LibRaw allocated (thumbnail or developed image).
struct MemImage(*mut sys::libraw_processed_image_t);

impl MemImage {
    fn new(p: *mut sys::libraw_processed_image_t, err: c_int, raw: &Raw) -> Result<MemImage> {
        if p.is_null() {
            raw.check(if err == 0 { -1 } else { err })?;
            bail!("LibRaw returned no image");
        }
        Ok(MemImage(p))
    }

    fn info(&self) -> &sys::libraw_processed_image_t {
        // SAFETY: non-null, owned by self.
        unsafe { &*self.0 }
    }

    fn data(&self) -> &[u8] {
        // SAFETY: `data` is a flexible array member of `data_size` bytes; the pointer is
        // derived from the allocation itself, not from the 1-element array field.
        unsafe {
            let base = std::ptr::addr_of!((*self.0).data).cast::<u8>();
            std::slice::from_raw_parts(base, (*self.0).data_size as usize)
        }
    }
}

impl Drop for MemImage {
    fn drop(&mut self) {
        // SAFETY: allocated by LibRaw's make_mem_* and freed once.
        unsafe { sys::libraw_dcraw_clear_mem(self.0) }
    }
}

fn text(b: &[c_char]) -> Option<String> {
    let bytes: Vec<u8> = b.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    let s = String::from_utf8_lossy(&bytes);
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// LibRaw's `flip` back to an EXIF orientation (inverse of dcraw's "50132467" table).
fn flip_to_exif(flip: c_int) -> u8 {
    match flip {
        1 => 2,
        2 => 4,
        3 => 3,
        4 => 5,
        5 => 8,
        6 => 6,
        7 => 7,
        _ => 1,
    }
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
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

fn utc_clock(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

/// LibRaw turns the camera's clock into a time_t with mktime(), i.e. as local time.
/// localtime_r undoes exactly that in any time zone (the helper runs with TZ=UTC, so
/// there are no daylight-saving gaps either).
fn clock_time(t: i64) -> String {
    let tt = t as libc::time_t;
    // SAFETY: zeroed `tm` is a valid out-parameter; localtime_r is thread-safe.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&tt, &mut tm) }.is_null() {
        return utc_clock(t);
    }
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
}

pub fn meta_op(args: &[OsString], out: &mut impl Write) -> Result<()> {
    let raw = Raw::open(&helper::path(args)?)?;
    let d = raw.d();
    let (ip, ot, s) = (&d.idata, &d.other, &d.sizes);
    let pos = |v: f32| (v.is_finite() && v > 0.0).then_some(v);
    let round2 = |v: f32| (v as f64 * 100.0).round() / 100.0;
    let g = &ot.parsed_gps;
    let (mut lat, mut lon, mut alt) = (None, None, None);
    if g.gpsparsed != 0 {
        let dms = |v: [f32; 3]| v[0] as f64 + v[1] as f64 / 60.0 + v[2] as f64 / 3600.0;
        let la = if g.latref as u8 == b'S' { -dms(g.latitude) } else { dms(g.latitude) };
        let lo = if g.longref as u8 == b'W' { -dms(g.longitude) } else { dms(g.longitude) };
        if la.is_finite() && lo.is_finite() && la.abs() <= 90.0 && lo.abs() <= 180.0 && !(la == 0.0 && lo == 0.0) {
            lat = Some(la);
            lon = Some(lo);
            alt = Some(g.altitude as f64).filter(|a| a.is_finite() && *a != 0.0).map(|a| if g.altref == 1 { -a } else { a });
        }
    }
    let m = LibrawMeta {
        // The names on the camera (LibRaw's "normalized" ones are the OEM's: a Leica
        // D-LUX 4 would show as a Panasonic DMC-LX3).
        make: text(&ip.make).or_else(|| text(&ip.normalized_make)),
        model: text(&ip.model).or_else(|| text(&ip.normalized_model)),
        lens: text(&d.lens.Lens).or_else(|| text(&d.lens.makernotes.Lens)),
        iso: pos(ot.iso_speed).map(|v| v.round() as u32),
        // Via the shortest decimal form: 1/1250 s stays 0.0008, not 0.000799999979.
        exposure: pos(ot.shutter).and_then(|v| v.to_string().parse::<f64>().ok()),
        fnum: pos(ot.aperture).map(round2),
        focal: pos(ot.focal_len).map(round2),
        // time_t is 32 bits on some targets.
        #[allow(clippy::useless_conversion)]
        taken: (ot.timestamp > 0).then(|| clock_time(i64::from(ot.timestamp))),
        artist: text(&ot.artist),
        lat,
        lon,
        alt,
        width: s.width as u32,
        height: s.height as u32,
        orientation: flip_to_exif(s.flip),
        xmp: None,
    };
    frames::json(out, b"META", &m)?;
    if ip.xmplen > 0 && ip.xmplen < 16 << 20 && !ip.xmpdata.is_null() {
        // SAFETY: LibRaw owns `xmplen` bytes at `xmpdata` until the handle closes.
        let x = unsafe { std::slice::from_raw_parts(ip.xmpdata as *const u8, ip.xmplen as usize) };
        frames::frame(out, b"XMP ", x)?;
    }
    Ok(())
}

pub fn preview_op(args: &[OsString], out: &mut impl Write) -> Result<()> {
    let path = helper::path(args)?;
    let min_long = helper::num(args, 1, "minimum size")?;
    let raw = Raw::open(&path)?;
    let tl = &raw.d().thumbs_list;
    let n = (tl.thumbcount.max(0) as usize).min(tl.thumblist.len());
    let items: Vec<(usize, usize, bool)> = (0..n)
        .map(|i| {
            let t = &tl.thumblist[i];
            (i, t.twidth.max(t.theight) as usize, t.tformat == sys::LibRaw_internal_thumbnail_formats_LIBRAW_INTERNAL_THUMBNAIL_JPEG)
        })
        .collect();
    // A JPEG passes through untouched: the smallest one big enough, else the largest.
    let pick = items.iter().filter(|t| t.2 && t.1 >= min_long).min_by_key(|t| t.1).or_else(|| items.iter().max_by_key(|t| (t.1, t.2)));
    // SAFETY: valid handle; the index comes from LibRaw's own list.
    let rc = match pick {
        Some(t) => unsafe { sys::libraw_unpack_thumb_ex(raw.0, t.0 as c_int) },
        None => unsafe { sys::libraw_unpack_thumb(raw.0) },
    };
    raw.check(rc)?;
    let mut err = 0;
    // SAFETY: valid handle after a successful unpack_thumb.
    let img = MemImage::new(unsafe { sys::libraw_dcraw_make_mem_thumb(raw.0, &mut err) }, err, &raw)?;
    let i = img.info();
    let (w, h) = (i.width as usize, i.height as usize);
    let data = img.data();
    if i.type_ == sys::LibRaw_image_formats_LIBRAW_IMAGE_JPEG {
        // make_mem_thumb leaves the size of a JPEG at 0; the unpacked thumbnail has it.
        let t = &raw.d().thumbnail;
        let (w, h) = if w > 0 && h > 0 { (w, h) } else { (t.twidth as usize, t.theight as usize) };
        frames::json(out, b"INFO", &serde_json::json!({"format": "jpeg", "w": w, "h": h}))?;
        frames::frame(out, b"DATA", data)?;
        return Ok(());
    }
    // A bitmap: 1 or 3 colours, 8 or 16 bits (native endian).
    let (colors, bytes) = (i.colors as usize, (i.bits as usize).div_ceil(8));
    if w == 0 || h == 0 || !(colors == 1 || colors == 3) || !(bytes == 1 || bytes == 2) || data.len() < w * h * colors * bytes {
        bail!("unsupported embedded preview ({} colours, {} bits)", i.colors, i.bits);
    }
    let mut px = Vec::with_capacity(w * h * 3);
    for p in 0..w * h {
        for c in 0..3 {
            let k = (p * colors + if colors == 3 { c } else { 0 }) * bytes;
            px.push(if bytes == 2 { (u16::from_ne_bytes([data[k], data[k + 1]]) >> 8) as u8 } else { data[k] });
        }
    }
    frames::json(out, b"INFO", &serde_json::json!({"format": "rgb8", "w": w, "h": h}))?;
    frames::frame(out, b"DATA", &px)?;
    Ok(())
}

/// DNG BaselineExposure; LibRaw reports -999 when the file has none.
fn baseline_ev(c: &sys::libraw_colordata_t) -> f32 {
    let b = c.dng_levels.baseline_exposure;
    if b.is_finite() && b.abs() < 20.0 { b } else { 0.0 }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Sensor layout for the server's own Bayer development, or None when the file needs
/// LibRaw's processing (non-Bayer, unusual geometry or black-level data).
fn bayer_layout(raw: &Raw) -> Option<BayerInfo> {
    let d = raw.d();
    let rd = &d.rawdata;
    let (ip, s, c) = (&d.idata, &d.sizes, &d.color);
    if rd.raw_image.is_null()
        || ip.filters <= 1000
        || ip.colors != 3
        || rd.ioparams.fuji_width != 0
        || (s.pixel_aspect - 1.0).abs() > 1e-3
        || !rd.ph1_cblack.is_null()
        || !rd.ph1_rblack.is_null()
    {
        return None;
    }
    // The CFA must repeat every 2x2 and hold R, B and two greens.
    for r in 0..8 {
        for col in 0..2 {
            if raw.color(r, col) != raw.color(r % 2, col % 2) {
                return None;
            }
        }
    }
    let cell = [[raw.color(0, 0), raw.color(0, 1)], [raw.color(1, 0), raw.color(1, 1)]];
    let mut count = [0; 4];
    for v in cell.iter().flatten() {
        count[usize::try_from(*v).ok().filter(|v| *v < 4)?] += 1;
    }
    if count[0] != 1 || count[2] != 1 || count[1] + count[3] != 2 {
        return None;
    }
    let ch = cell.map(|row| row.map(|v| if v == 3 { 1 } else { v as u8 }));

    let (raw_w, raw_h) = (s.raw_width as usize, s.raw_height as usize);
    let (top, left) = (s.top_margin as usize, s.left_margin as usize);
    let w = (s.width as usize).min(raw_w.saturating_sub(left)) & !1;
    let h = (s.height as usize).min(raw_h.saturating_sub(top)) & !1;
    if w < 2 || h < 2 || (s.raw_pitch as usize) / 2 < raw_w {
        return None;
    }

    // Black: common + per colour + an optional repeating pattern, combined into one
    // pattern from the top-left corner of the visible area.
    let (pr, pc) = (c.cblack[4] as usize, c.cblack[5] as usize);
    let pattern = pr > 0 && pc > 0 && pr * pc <= c.cblack.len() - 6;
    let (rows, cols) = if pattern { (pr * 2 / gcd(pr, 2), pc * 2 / gcd(pc, 2)) } else { (2, 2) };
    if rows > 64 || cols > 64 {
        return None;
    }
    let mut black = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        for col in 0..cols {
            let mut b = c.black as f64 + c.cblack[raw.color(r, col).clamp(0, 3) as usize] as f64;
            if pattern {
                b += c.cblack[6 + (r % pr) * pc + col % pc] as f64;
            }
            black.push(b as f32);
        }
    }
    let wb = |m: [f32; 4]| -> Option<[f32; 3]> {
        let ok = |v: f32| v.is_finite() && v > 0.0;
        (ok(m[0]) && ok(m[1]) && ok(m[2])).then(|| [m[0] / m[1], 1.0, m[2] / m[1]])
    };
    let mut m = [[0f32; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = c.rgb_cam[i][j];
        }
    }
    if m.iter().flatten().any(|v| !v.is_finite()) || m.iter().flatten().all(|v| *v == 0.0) {
        m = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    Some(BayerInfo {
        w,
        h,
        ch,
        black_rows: rows,
        black_cols: cols,
        black,
        white: if c.maximum > 0 { c.maximum as f32 } else { 65535.0 },
        as_shot: wb(c.cam_mul),
        daylight: wb(c.pre_mul),
        cam_to_srgb: m,
        baseline_ev: baseline_ev(c),
    })
}

pub fn raw_op(args: &[OsString], out: &mut impl Write) -> Result<()> {
    let path = helper::path(args)?;
    let wb = helper::word(args, 1, "white balance")?;
    let mut raw = Raw::open(&path)?;
    // SAFETY: valid handle.
    raw.check(unsafe { sys::libraw_unpack(raw.0) })?;
    if let Some(info) = bayer_layout(&raw) {
        let d = raw.d();
        let s = &d.sizes;
        let pitch = s.raw_pitch as usize / 2;
        let (top, left) = (s.top_margin as usize, s.left_margin as usize);
        // SAFETY: LibRaw allocated raw_pitch * raw_height bytes for raw_image.
        let src = unsafe { std::slice::from_raw_parts(d.rawdata.raw_image, pitch * s.raw_height as usize) };
        frames::json(out, b"INFO", &RawInfo::Bayer(info.clone()))?;
        frames::frame_header(out, b"DATA", (info.w * info.h * 2) as u64)?;
        for y in 0..info.h {
            let start = (top + y) * pitch + left;
            frames::write_u16s(out, &src[start..start + info.w])?;
        }
        return Ok(());
    }

    // LibRaw develops it: half size, linear sRGB, 16 bits, no rotation, clipped highlights.
    let baseline_ev = baseline_ev(&raw.d().color);
    {
        let p = &mut raw.d_mut().params;
        p.half_size = 1;
        p.output_bps = 16;
        p.gamm[0] = 1.0;
        p.gamm[1] = 1.0;
        p.no_auto_bright = 1;
        p.output_color = 1;
        p.highlight = 0;
        p.user_flip = 0;
        p.user_qual = 0;
        p.use_camera_wb = (wb == "asshot") as c_int;
        p.use_auto_wb = (wb == "auto") as c_int;
    }
    // SAFETY: valid handle after unpack.
    raw.check(unsafe { sys::libraw_dcraw_process(raw.0) })?;
    let mut err = 0;
    // SAFETY: valid handle after a successful dcraw_process.
    let img = MemImage::new(unsafe { sys::libraw_dcraw_make_mem_image(raw.0, &mut err) }, err, &raw)?;
    let i = img.info();
    let (w, h) = (i.width as usize, i.height as usize);
    let data = img.data();
    if i.type_ != sys::LibRaw_image_formats_LIBRAW_IMAGE_BITMAP || i.colors != 3 || i.bits != 16 || w == 0 || h == 0 || data.len() < w * h * 6 {
        bail!("LibRaw produced an unexpected image ({} colours, {} bits)", i.colors, i.bits);
    }
    frames::json(out, b"INFO", &RawInfo::Rgb16(RgbInfo { w, h, baseline_ev }))?;
    frames::frame_header(out, b"DATA", (w * h * 6) as u64)?;
    let samples: Vec<u16> = data[..w * h * 6].chunks_exact(2).map(|b| u16::from_ne_bytes([b[0], b[1]])).collect();
    frames::write_u16s(out, &samples)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_clock() {
        assert_eq!(utc_clock(0), "1970-01-01T00:00:00");
        assert_eq!(utc_clock(1_370_690_080), "2013-06-08T11:14:40");
        assert_eq!(crate::model::taken_to_secs(&utc_clock(1_745_776_472)), Some(1_745_776_472));
        assert_eq!(utc_clock(951_782_400), "2000-02-29T00:00:00");
        // What LibRaw does with a capture time, undone in whatever zone the test runs in.
        // SAFETY: plain libc calls on a local struct.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        (tm.tm_year, tm.tm_mon, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec, tm.tm_isdst) = (124, 6, 4, 18, 30, 5, -1);
        let t = unsafe { libc::mktime(&mut tm) };
        assert_eq!(clock_time(t as i64), "2024-07-04T18:30:05");
    }

    #[test]
    fn flips() {
        // dcraw: flip = "50132467"[exif orientation]
        let table = b"50132467";
        for exif in 1..=8u8 {
            let flip = (table[exif as usize & 7] - b'0') as c_int;
            assert_eq!(flip_to_exif(flip), exif);
        }
    }
}
