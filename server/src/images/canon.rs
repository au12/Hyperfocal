//! Canon maker-note facts that FlashRaw parses but does not apply to the frame.

use flashraw_core::tiff::{Tiff, tags};
use flashraw_formats::canon::cr3::container::{Cr3, Media};
use flashraw_formats::canon::makernote::CanonMakernote;

/// White levels from a Canon maker note (ColorData).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WhiteLevels {
    /// NormalWhiteLevel: the level Canon's own rendering (and FastRawViewer's) maps to white.
    pub normal: Option<u16>,
    /// SpecularWhiteLevel: the raw value where the sensor actually saturates.
    pub specular: Option<u16>,
}

/// The white levels of a CR2/CR3. The raw header states the nominal 14-bit maximum, but
/// Canon's 1/3-stop "pulled" ISOs (160, 320, 640, 1250 ...) and some others clip well
/// below it (an R6 at ISO 1250: specular 14888, normal 13535 of 16383). LibRaw reports
/// the same values (`linear_max`, `makernotes.canon`). `None` for other files.
pub fn white_levels(data: &[u8]) -> Option<WhiteLevels> {
    let notes = if is_cr3(data) {
        cr3_notes(data)
    } else if is_cr2(data) {
        cr2_note(data).into_iter().collect()
    } else {
        return None;
    };
    notes.iter().find_map(|n| {
        let c = n.color.as_ref()?;
        let w = WhiteLevels { normal: c.normal_white.filter(|&v| v > 0), specular: c.specular_white.filter(|&v| v > 0) };
        (w.normal.is_some() || w.specular.is_some()).then_some(w)
    })
}

fn is_cr3(data: &[u8]) -> bool {
    flashraw_formats::canon::cr3::probe(data)
}

fn is_cr2(data: &[u8]) -> bool {
    data.len() > 16 && &data[0..4] == b"II*\0" && &data[8..11] == b"CR\x02"
}

fn first_ifd_note(tiff_bytes: &[u8]) -> Option<CanonMakernote> {
    let tiff = Tiff::parse(tiff_bytes).ok()?;
    tiff.ifds.first().map(CanonMakernote::parse)
}

fn le32(d: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?) as usize)
}

/// The maker note of `CMT3` and those of the timed-metadata track (where the R5/R6
/// generation keeps ColorData). A timed-metadata sample is a run of records (size, type,
/// six more header bytes, payload); records 7 and 8 hold items (length, tag, payload),
/// and item 0x927C is a complete maker-note TIFF stream.
fn cr3_notes(data: &[u8]) -> Vec<CanonMakernote> {
    let Ok(cr3) = Cr3::parse(data) else { return Vec::new() };
    let mut notes: Vec<CanonMakernote> = cr3.cmt[2].and_then(first_ifd_note).into_iter().collect();
    for track in cr3.tracks.iter().filter(|t| t.media == Media::TimedMetadata) {
        let Some(sample) = track.sample(0).and_then(|(offset, size)| {
            let start = usize::try_from(offset).ok()?;
            data.get(start..start.checked_add(usize::try_from(size).ok()?)?)
        }) else {
            continue;
        };
        let mut p = 0;
        while p + 6 <= sample.len() {
            let size = le32(sample, p).unwrap_or(0);
            let ty = u16::from_le_bytes([sample[p + 4], sample[p + 5]]);
            let Some(end) = p.checked_add(size).filter(|&e| size != 0 && e <= sample.len()) else { break };
            if (ty == 7 || ty == 8) && size > 12 {
                let record = &sample[p + 12..end];
                let mut q = 0;
                while q + 9 <= record.len() {
                    let (Some(len), Some(tag)) = (le32(record, q), le32(record, q + 4)) else { break };
                    let Some(item_end) = q.checked_add(len).filter(|&e| len >= 8 && e <= record.len()) else { break };
                    if tag == tags::MAKER_NOTE as usize
                        && let Some(n) = first_ifd_note(&record[q + 8..item_end])
                    {
                        notes.push(n);
                    }
                    q = item_end;
                }
            }
            p = end;
        }
    }
    notes
}

/// The maker note of a CR2: EXIF tag 0x927C, an IFD whose offsets count from the file start.
fn cr2_note(data: &[u8]) -> Option<CanonMakernote> {
    let tiff = Tiff::parse(data).ok()?;
    let entry = tiff.all_ifds().into_iter().find_map(|ifd| ifd.exif.as_ref()?.get(tags::MAKER_NOTE))?;
    let ifd = tiff.sub_ifd_at(entry.value_field).ok()?;
    Some(CanonMakernote::parse(&ifd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_files_have_none() {
        let dng = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tiny.dng")).unwrap();
        assert_eq!(white_levels(&dng), None);
        assert_eq!(white_levels(b"II*\0\x08\0\0\0CR\x02\0garbage that is not a TIFF"), None);
        assert_eq!(white_levels(&[]), None);
    }

    /// `HYPERFOCAL_TEST_CR3=path/to/file.CR3 HYPERFOCAL_TEST_CR3_WHITE=normal,specular`
    /// checks a real file (none ship with the repository: they are tens of megabytes).
    #[test]
    fn real_cr3_from_env() {
        let (Ok(path), Ok(want)) = (std::env::var("HYPERFOCAL_TEST_CR3"), std::env::var("HYPERFOCAL_TEST_CR3_WHITE")) else { return };
        let w = white_levels(&std::fs::read(path).unwrap()).expect("white levels");
        let want: Vec<u16> = want.split(',').map(|v| v.trim().parse().unwrap()).collect();
        assert_eq!((w.normal, w.specular), (Some(want[0]), Some(want[1])));
    }
}
