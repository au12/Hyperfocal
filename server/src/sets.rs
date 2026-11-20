//! Which files make up a photo.
//!
//! One set of rules, used by the scanner (which sidecar a photo reads), the index
//! (which files fold into a RAW), the sidecar writer (which sidecar an edit goes to),
//! the reject/restore mover (which files move together) and the info panel, so they
//! can never disagree:
//!
//! * A JPEG or HEIF with the same name as a RAW in the same folder is folded into
//!   that RAW (a RAW+JPEG pair). With two RAWs of the same name, a camera RAW is
//!   preferred over a DNG.
//! * Of the photos that share a name, one owns `name.xmp` (the sidecar Adobe and
//!   Excire use) and the other same-name files that are not photos (`.WAV` voice
//!   memos and the like): a camera RAW first, then a DNG, JPEG, HEIF, TIFF and PNG.
//!   Every other photo with that name uses its own `name.ext.xmp`, so an edit of a
//!   TIFF never lands in its RAW's sidecar.
//! * A file named after a photo's full name (`IMG_1.CR3.xmp`, `IMG_1.JPG.pp3`)
//!   belongs to that photo, whatever else its name could match.
//! * Ownership is decided across a folder and its reject folder together, so
//!   rejecting or restoring a photo never changes which sidecar any photo uses. Two
//!   different photos with the identical file name in those folders (a camera
//!   counter that started over) are unrelated.
//! * A reject or restore moves the photo, the files folded into it, the files named
//!   after them and, for the owner, the same-name files that are not photos.

use crate::model::{Kind, kind_of_ext, split_name};
use anyhow::{Result, bail};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// File names in a folder (`local`) and in the folders it shares sidecars with
/// (`partners`): its home folder and the reject folders in that home.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    local: Vec<String>,
    partners: Vec<String>,
}

impl Listing {
    /// Record a rename of a file in this folder.
    pub fn rename(&mut self, from: &str, to: &str) {
        self.local.retain(|n| n != from);
        self.local.push(to.to_string());
        self.local.sort();
        self.local.dedup();
    }

    pub fn new(mut local: Vec<String>, partners: Vec<String>) -> Listing {
        local.sort();
        local.dedup();
        Listing { local, partners }
    }

    /// Read a folder and its partners from disk.
    pub fn read(dir: &Path, reject_folder: bool, is_reject_name: &dyn Fn(&str) -> bool) -> std::io::Result<Listing> {
        let (local, rejects) = entries(dir, is_reject_name)?;
        let partners = partner_names(dir, reject_folder, &rejects, is_reject_name)?;
        Ok(Listing::new(local, partners))
    }
}

/// Regular, non-hidden files of a folder, and its reject subfolders.
pub fn entries(dir: &Path, is_reject_name: &dyn Fn(&str) -> bool) -> std::io::Result<(Vec<String>, Vec<PathBuf>)> {
    let mut files = Vec::new();
    let mut rejects = Vec::new();
    for ent in std::fs::read_dir(dir)?.flatten() {
        let Some(name) = ent.file_name().to_str().map(str::to_string) else { continue };
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = ent.file_type() else { continue };
        if ft.is_file() {
            files.push(name);
        } else if ft.is_dir() && is_reject_name(&name) {
            rejects.push(dir.join(name));
        }
    }
    Ok((files, rejects))
}

/// File names in the partner folders of `dir`: for a reject folder, its home folder
/// and the home's other reject folders; for any other folder, its reject folders
/// (`own_rejects`, as found by [`entries`]).
pub fn partner_names(
    dir: &Path,
    reject_folder: bool,
    own_rejects: &[PathBuf],
    is_reject_name: &dyn Fn(&str) -> bool,
) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    if reject_folder {
        let Some(home) = dir.parent() else { return Ok(names) };
        let (files, rejects) = entries(home, is_reject_name)?;
        names.extend(files);
        for r in rejects.iter().filter(|r| r.file_name() != dir.file_name()) {
            names.extend(entries(r, is_reject_name)?.0);
        }
    } else {
        for r in own_rejects {
            names.extend(entries(r, is_reject_name)?.0);
        }
    }
    Ok(names)
}

/// What a photo file is to the files around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// Its own photo, and the owner of `name.xmp` and the other same-name files.
    Owner,
    /// Its own photo; another photo with the same name owns `name.xmp`.
    Separate,
    /// The JPEG or HEIF of a RAW+JPEG pair, folded into `raw`.
    Paired { raw: String },
}

/// Photo kind and rank for owning the shared files of its name (lower wins).
fn classify(name: &str) -> Option<(Kind, u8)> {
    let (_, ext) = split_name(name);
    let kind = kind_of_ext(&ext)?;
    let rank = match kind {
        Kind::Raw if ext == "dng" => 1,
        Kind::Raw => 0,
        Kind::Jpeg => 2,
        Kind::Heic => 3,
        Kind::Tiff => 4,
        Kind::Png => 5,
    };
    Some((kind, rank))
}

/// Total order of photos that share a name: rank, then name (case-insensitively first).
type Key = (u8, String, String);

fn key(name: &str, rank: u8) -> Key {
    (rank, name.to_lowercase(), name.to_string())
}

fn keep_min(best: &mut HashMap<String, (Key, usize)>, stem: &str, k: Key, i: usize) {
    match best.get(stem) {
        Some((b, _)) if *b <= k => {}
        _ => {
            best.insert(stem.to_string(), (k, i));
        }
    }
}

/// The photo whose full name `lower` extends: the longest `P` with `lower` = `P.…`.
fn named_after(lower: &str, photos: &HashMap<String, usize>) -> Option<usize> {
    lower.match_indices('.').rev().find_map(|(pos, _)| photos.get(&lower[..pos]).copied())
}

struct LocalFile {
    /// Lowercase stem.
    stem: String,
    /// Kind and rank, for photo files.
    photo: Option<(Kind, u8)>,
    /// For other files: the photo here whose full name they extend.
    named_after: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct Group {
    /// The RAW that JPEG/HEIF files of this name fold into.
    raw: Option<usize>,
    /// The owner of the shared files, when it is in this folder (it can also be in
    /// the reject folder or the home folder).
    owner: Option<usize>,
    /// Whether a photo with this name is also in the reject folder or the home folder.
    elsewhere: bool,
}

/// The rules applied to one folder.
pub struct Plan<'a> {
    local: &'a [String],
    files: Vec<LocalFile>,
    by_name: HashMap<&'a str, usize>,
    /// Lowercase name -> first matching file (the listing is sorted).
    by_lower: HashMap<String, usize>,
    /// Lowercase name -> photo, for photo files.
    photos: HashMap<String, usize>,
    /// Lowercase stem -> group, for every name that has a photo here.
    groups: HashMap<String, Group>,
    /// Lowercase stem -> the photos with that name here.
    stem_photos: HashMap<String, Vec<usize>>,
}

impl<'a> Plan<'a> {
    pub fn new(listing: &'a Listing) -> Plan<'a> {
        let local = &listing.local[..];
        let mut files = Vec::with_capacity(local.len());
        let mut by_name = HashMap::with_capacity(local.len());
        let mut by_lower = HashMap::with_capacity(local.len());
        let mut photos: HashMap<String, usize> = HashMap::new();
        let mut best: HashMap<String, (Key, usize)> = HashMap::new();
        let mut best_raw: HashMap<String, (Key, usize)> = HashMap::new();
        let mut stem_photos: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, name) in local.iter().enumerate() {
            let lower = name.to_lowercase();
            let stem = split_name(name).0.to_lowercase();
            let photo = classify(name);
            if let Some((kind, rank)) = photo {
                keep_min(&mut best, &stem, key(name, rank), i);
                if kind == Kind::Raw {
                    keep_min(&mut best_raw, &stem, key(name, rank), i);
                }
                photos.entry(lower.clone()).or_insert(i);
                stem_photos.entry(stem.clone()).or_default().push(i);
            }
            by_name.insert(name.as_str(), i);
            by_lower.entry(lower).or_insert(i);
            files.push(LocalFile { stem, photo, named_after: None });
        }
        for (i, f) in files.iter_mut().enumerate() {
            if f.photo.is_none() {
                f.named_after = named_after(&local[i].to_lowercase(), &photos);
            }
        }
        // The best same-name photo in the partner folders. A file with the identical
        // name is a different photo (it could never move here), so it does not count.
        let mut best_partner: HashMap<&str, Key> = HashMap::new();
        for name in &listing.partners {
            let Some((_, rank)) = classify(name) else { continue };
            if photos.contains_key(&name.to_lowercase()) {
                continue;
            }
            let stem = split_name(name).0.to_lowercase();
            let Some((stem, _)) = best.get_key_value(&stem) else { continue };
            let k = key(name, rank);
            if best_partner.get(stem.as_str()).is_none_or(|b| k < *b) {
                best_partner.insert(stem.as_str(), k);
            }
        }
        let groups = best
            .iter()
            .map(|(stem, (k, i))| {
                let owner = match best_partner.get(stem.as_str()) {
                    Some(p) if p < k => None,
                    _ => Some(*i),
                };
                let elsewhere = best_partner.contains_key(stem.as_str());
                (stem.clone(), Group { raw: best_raw.get(stem).map(|(_, j)| *j), owner, elsewhere })
            })
            .collect();
        Plan { local, files, by_name, by_lower, photos, groups, stem_photos }
    }

    fn group(&self, i: usize) -> Option<Group> {
        self.files[i].photo?;
        self.groups.get(&self.files[i].stem).copied()
    }

    /// The role of a photo file in this folder (None for other files and unknown names).
    pub fn role(&self, name: &str) -> Option<Role> {
        let &i = self.by_name.get(name)?;
        let (kind, _) = self.files[i].photo?;
        let g = self.group(i)?;
        if matches!(kind, Kind::Jpeg | Kind::Heic)
            && let Some(r) = g.raw
        {
            return Some(Role::Paired { raw: self.local[r].clone() });
        }
        Some(if g.owner == Some(i) { Role::Owner } else { Role::Separate })
    }

    /// The JPEG/HEIF files folded into a RAW, in badge order (JPEG before HEIF).
    pub fn paired(&self, raw: &str) -> Vec<&'a str> {
        let Some(&i) = self.by_name.get(raw) else { return Vec::new() };
        if self.group(i).and_then(|g| g.raw) != Some(i) {
            return Vec::new();
        }
        let mut out: Vec<(Key, &'a str)> = self.stem_photos[&self.files[i].stem]
            .iter()
            .filter_map(|&j| match self.files[j].photo {
                Some((Kind::Jpeg | Kind::Heic, rank)) => Some((key(&self.local[j], rank), self.local[j].as_str())),
                _ => None,
            })
            .collect();
        out.sort();
        out.into_iter().map(|(_, n)| n).collect()
    }

    /// A photo's own sidecar, `name.ext.xmp`, if it exists.
    fn own_sidecar(&self, name: &str) -> Option<&'a str> {
        self.by_lower.get(&format!("{}.xmp", name.to_lowercase())).map(|&i| self.local[i].as_str())
    }

    /// The shared sidecar name of a photo's name (`IMG_1.xmp`), unless that name is
    /// another photo's own sidecar (`IMG_1.JPG.xmp` next to an `IMG_1.JPG.tif`).
    /// Returns the existing file, or the name to create.
    fn shared_sidecar(&self, name: &str) -> Option<String> {
        let stem = split_name(name).0;
        let lower = format!("{}.xmp", stem.to_lowercase());
        if named_after(&lower, &self.photos).is_some() {
            return None;
        }
        Some(self.by_lower.get(&lower).map_or_else(|| format!("{stem}.xmp"), |&i| self.local[i].clone()))
    }

    fn exists(&self, name: &str) -> bool {
        self.by_lower.contains_key(&name.to_lowercase())
    }

    /// The sidecar a photo reads, if it has one: the owner reads `name.xmp` (or its
    /// own `name.ext.xmp` when there is none), every other photo its own `name.ext.xmp`.
    pub fn read_sidecar(&self, name: &str) -> Option<String> {
        let shared = match self.role(name)? {
            Role::Owner => self.shared_sidecar(name).filter(|s| self.exists(s)),
            Role::Separate | Role::Paired { .. } => None,
        };
        shared.or_else(|| self.own_sidecar(name).map(str::to_string))
    }

    /// The sidecar a metadata edit of a photo goes to: the one it reads, or when it has
    /// none, a new `name.xmp` for the owner and `name.ext.xmp` for a separate photo with
    /// the same name.
    pub fn write_sidecar(&self, name: &str) -> Result<String> {
        let shared = match self.role(name) {
            None => bail!("{name} is no longer in its folder"),
            Some(Role::Paired { raw }) => bail!("{name} is part of the RAW+JPEG pair of {raw}; edit {raw} instead"),
            Some(Role::Owner) => self.shared_sidecar(name),
            Some(Role::Separate) => None,
        };
        if let Some(current) = self.read_sidecar(name) {
            return Ok(current);
        }
        Ok(shared.unwrap_or_else(|| format!("{name}.xmp")))
    }

    /// The rename that gives a photo's darktable/digiKam-style sidecar (`IMG_1.CR3.xmp`)
    /// the standard name (`IMG_1.xmp`). Only for a photo that is alone with its name
    /// (apart from the JPEG/HEIF folded into it) here and in the reject or home folder,
    /// and only when the standard name is free, so no other photo's metadata can ever
    /// end up in it.
    pub fn standard_rename(&self, name: &str) -> Option<(String, String)> {
        if self.role(name)? != Role::Owner {
            return None;
        }
        let i = self.by_name[name];
        let stem = &self.files[i].stem;
        if self.groups[stem].elsewhere {
            return None;
        }
        let alone = self.stem_photos[stem]
            .iter()
            .all(|&j| j == i || matches!(self.role(&self.local[j]), Some(Role::Paired { .. })));
        if !alone {
            return None;
        }
        let own = self.own_sidecar(name)?;
        let shared = self.shared_sidecar(name)?;
        (!self.exists(&shared)).then(|| (own.to_string(), shared))
    }

    /// [`Plan::standard_rename`] for every photo in the folder: `(photo, from, to)`.
    pub fn standard_renames(&self) -> Vec<(&'a str, String, String)> {
        self.local
            .iter()
            .filter_map(|n| self.standard_rename(n).map(|(from, to)| (n.as_str(), from, to)))
            .collect()
    }

    /// The files that move with a photo on reject and restore, sorted.
    pub fn move_set(&self, name: &str) -> Result<Vec<String>> {
        let role = match self.role(name) {
            None => bail!("{name} is no longer in its folder"),
            Some(Role::Paired { raw }) => bail!("{name} is part of the RAW+JPEG pair of {raw}; reject or restore {raw} instead"),
            Some(r) => r,
        };
        let i = self.by_name[name];
        let mut members: Vec<usize> = vec![i];
        members.extend(self.paired(name).into_iter().map(|n| self.by_name[n]).filter(|&j| j != i));
        let mut out: BTreeSet<String> = members.iter().map(|&m| self.local[m].clone()).collect();
        for (j, f) in self.files.iter().enumerate() {
            if f.photo.is_some() {
                continue;
            }
            let own = f.named_after.is_some_and(|p| members.contains(&p));
            let shared = role == Role::Owner && f.named_after.is_none() && f.stem == self.files[i].stem;
            if own || shared {
                out.insert(self.local[j].clone());
            }
        }
        Ok(out.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(local: &[&str], partners: &[&str]) -> Listing {
        Listing::new(local.iter().map(|s| s.to_string()).collect(), partners.iter().map(|s| s.to_string()).collect())
    }

    fn paired(raw: &str) -> Option<Role> {
        Some(Role::Paired { raw: raw.into() })
    }

    #[test]
    fn raw_jpeg_pair() {
        let l = listing(
            &["IMG_1.CR3", "IMG_1.JPG", "IMG_1.xmp", "IMG_1.CR3.pp3", "IMG_1.JPG.xmp", "IMG_10.CR3", "IMG_1.WAV", "other.txt"],
            &[],
        );
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.CR3"), Some(Role::Owner));
        assert_eq!(p.role("IMG_1.JPG"), paired("IMG_1.CR3"));
        assert_eq!(p.role("IMG_10.CR3"), Some(Role::Owner));
        assert_eq!(p.role("IMG_1.xmp"), None);
        assert_eq!(p.role("missing.CR3"), None);
        assert_eq!(p.paired("IMG_1.CR3"), vec!["IMG_1.JPG"]);
        assert!(p.paired("IMG_10.CR3").is_empty());
        assert_eq!(p.read_sidecar("IMG_1.CR3").as_deref(), Some("IMG_1.xmp"));
        assert_eq!(p.write_sidecar("IMG_1.CR3").unwrap(), "IMG_1.xmp");
        assert_eq!(p.write_sidecar("IMG_10.CR3").unwrap(), "IMG_10.xmp");
        assert_eq!(
            p.move_set("IMG_1.CR3").unwrap(),
            vec!["IMG_1.CR3", "IMG_1.CR3.pp3", "IMG_1.JPG", "IMG_1.JPG.xmp", "IMG_1.WAV", "IMG_1.xmp"]
        );
        assert_eq!(p.move_set("IMG_10.CR3").unwrap(), vec!["IMG_10.CR3"]);
        // The JPEG half is never edited or moved on its own.
        assert!(p.move_set("IMG_1.JPG").is_err());
        assert!(p.write_sidecar("IMG_1.JPG").is_err());
        assert!(p.move_set("missing.CR3").is_err());
    }

    #[test]
    fn raw_jpeg_heif_set() {
        let l = listing(&["IMG_1.CR3", "IMG_1.HIF", "IMG_1.JPG"], &[]);
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.HIF"), paired("IMG_1.CR3"));
        assert_eq!(p.paired("IMG_1.CR3"), vec!["IMG_1.JPG", "IMG_1.HIF"]);
        assert_eq!(p.move_set("IMG_1.CR3").unwrap(), vec!["IMG_1.CR3", "IMG_1.HIF", "IMG_1.JPG"]);
    }

    #[test]
    fn same_name_tiff_keeps_to_itself() {
        let l = listing(&["IMG_1.CR3", "IMG_1.JPG", "IMG_1.TIF", "IMG_1.xmp", "IMG_1.TIF.xmp", "IMG_1.WAV"], &[]);
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.CR3"), Some(Role::Owner));
        assert_eq!(p.role("IMG_1.TIF"), Some(Role::Separate));
        assert_eq!(p.read_sidecar("IMG_1.TIF").as_deref(), Some("IMG_1.TIF.xmp"));
        assert_eq!(p.write_sidecar("IMG_1.TIF").unwrap(), "IMG_1.TIF.xmp");
        assert_eq!(p.move_set("IMG_1.TIF").unwrap(), vec!["IMG_1.TIF", "IMG_1.TIF.xmp"]);
        assert_eq!(p.move_set("IMG_1.CR3").unwrap(), vec!["IMG_1.CR3", "IMG_1.JPG", "IMG_1.WAV", "IMG_1.xmp"]);
        // Without its own sidecar, the TIFF reads none (not the RAW's) and gets a new one.
        let l = listing(&["IMG_1.CR3", "IMG_1.TIF", "IMG_1.xmp"], &[]);
        let p = Plan::new(&l);
        assert_eq!(p.read_sidecar("IMG_1.TIF"), None);
        assert_eq!(p.write_sidecar("IMG_1.TIF").unwrap(), "IMG_1.TIF.xmp");
    }

    #[test]
    fn camera_raw_beats_dng() {
        let l = listing(&["IMG_1.DNG", "IMG_1.CR3", "IMG_1.JPG", "IMG_1.xmp", "IMG_1.DNG.xmp"], &[]);
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.CR3"), Some(Role::Owner));
        assert_eq!(p.role("IMG_1.DNG"), Some(Role::Separate));
        assert_eq!(p.role("IMG_1.JPG"), paired("IMG_1.CR3"));
        assert!(p.paired("IMG_1.DNG").is_empty());
        assert_eq!(p.read_sidecar("IMG_1.DNG").as_deref(), Some("IMG_1.DNG.xmp"));
        assert_eq!(p.move_set("IMG_1.DNG").unwrap(), vec!["IMG_1.DNG", "IMG_1.DNG.xmp"]);
        assert_eq!(p.move_set("IMG_1.CR3").unwrap(), vec!["IMG_1.CR3", "IMG_1.JPG", "IMG_1.xmp"]);
        // A camera that records DNG: the DNG is the RAW of its pair.
        let l = listing(&["R0000001.DNG", "R0000001.JPG", "R0000001.xmp"], &[]);
        let p = Plan::new(&l);
        assert_eq!(p.role("R0000001.DNG"), Some(Role::Owner));
        assert_eq!(p.role("R0000001.JPG"), paired("R0000001.DNG"));
    }

    #[test]
    fn without_a_raw() {
        let l = listing(&["IMG_1.HEIC", "IMG_1.JPG", "IMG_1.PNG", "IMG_1.xmp"], &[]);
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.JPG"), Some(Role::Owner));
        assert_eq!(p.role("IMG_1.HEIC"), Some(Role::Separate));
        assert_eq!(p.role("IMG_1.PNG"), Some(Role::Separate));
        assert_eq!(p.read_sidecar("IMG_1.JPG").as_deref(), Some("IMG_1.xmp"));
        assert_eq!(p.read_sidecar("IMG_1.HEIC"), None);
        assert_eq!(p.move_set("IMG_1.HEIC").unwrap(), vec!["IMG_1.HEIC"]);
        assert_eq!(p.move_set("IMG_1.JPG").unwrap(), vec!["IMG_1.JPG", "IMG_1.xmp"]);
    }

    #[test]
    fn ownership_spans_the_reject_folder() {
        // The RAW is rejected; the TIFF left behind must not take over IMG_1.xmp.
        let l = listing(&["IMG_1.TIF", "IMG_1.TIF.xmp"], &["IMG_1.CR3", "IMG_1.JPG", "IMG_1.xmp"]);
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.TIF"), Some(Role::Separate));
        assert_eq!(p.write_sidecar("IMG_1.TIF").unwrap(), "IMG_1.TIF.xmp");
        // ... and seen from the reject folder, the RAW still owns it.
        let l = listing(&["IMG_1.CR3", "IMG_1.JPG", "IMG_1.xmp"], &["IMG_1.TIF", "IMG_1.TIF.xmp"]);
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.CR3"), Some(Role::Owner));
        assert_eq!(p.move_set("IMG_1.CR3").unwrap(), vec!["IMG_1.CR3", "IMG_1.JPG", "IMG_1.xmp"]);
        // A rejected TIFF without a sidecar gets its own, never IMG_1.xmp.
        let l = listing(&["IMG_1.TIF"], &["IMG_1.CR3"]);
        assert_eq!(Plan::new(&l).write_sidecar("IMG_1.TIF").unwrap(), "IMG_1.TIF.xmp");
        // Same-name files there that are not photos change nothing.
        let l = listing(&["IMG_1.TIF"], &["IMG_1.xmp", "IMG_1.WAV"]);
        assert_eq!(Plan::new(&l).role("IMG_1.TIF"), Some(Role::Owner));
    }

    #[test]
    fn identical_names_elsewhere_are_other_photos() {
        // A camera counter that started over: a different IMG_1.CR3 in the reject folder.
        let l = listing(&["IMG_1.CR3", "IMG_1.xmp"], &["IMG_1.CR3", "IMG_1.xmp"]);
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.CR3"), Some(Role::Owner));
        assert_eq!(p.read_sidecar("IMG_1.CR3").as_deref(), Some("IMG_1.xmp"));
        // Both sides agree: here the TIFF defers to the other folder's RAW, and that
        // folder's own TIFF (same name as ours) is ignored.
        let l = listing(&["IMG_1.TIF"], &["IMG_1.CR3", "IMG_1.TIF"]);
        assert_eq!(Plan::new(&l).role("IMG_1.TIF"), Some(Role::Separate));
        let l = listing(&["IMG_1.CR3", "IMG_1.TIF"], &["IMG_1.TIF"]);
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.CR3"), Some(Role::Owner));
        assert_eq!(p.role("IMG_1.TIF"), Some(Role::Separate));
    }

    #[test]
    fn photos_are_never_swept_up_as_sidecars() {
        // IMG_1.JPG.tif is a photo of its own (stem IMG_1.JPG), not a file of IMG_1.JPG.
        let l = listing(
            &["IMG_1.JPG", "IMG_1.JPG.tif", "IMG_1.JPG.xmp", "IMG_1.JPG.tif.xmp", "IMG_1.edit.jpg", "IMG_1.edit.xmp"],
            &[],
        );
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.JPG.tif"), Some(Role::Owner));
        assert_eq!(p.role("IMG_1.edit.jpg"), Some(Role::Owner));
        assert_eq!(p.move_set("IMG_1.JPG").unwrap(), vec!["IMG_1.JPG", "IMG_1.JPG.xmp"]);
        assert_eq!(p.move_set("IMG_1.JPG.tif").unwrap(), vec!["IMG_1.JPG.tif", "IMG_1.JPG.tif.xmp"]);
        assert_eq!(p.move_set("IMG_1.edit.jpg").unwrap(), vec!["IMG_1.edit.jpg", "IMG_1.edit.xmp"]);
        // IMG_1.JPG.xmp is the JPEG's own sidecar, so the TIFF never reads or writes it.
        assert_eq!(p.read_sidecar("IMG_1.JPG").as_deref(), Some("IMG_1.JPG.xmp"));
        assert_eq!(p.read_sidecar("IMG_1.JPG.tif").as_deref(), Some("IMG_1.JPG.tif.xmp"));
        assert_eq!(p.write_sidecar("IMG_1.JPG.tif").unwrap(), "IMG_1.JPG.tif.xmp");
        let l = listing(&["IMG_1.JPG", "IMG_1.JPG.tif"], &[]);
        assert_eq!(Plan::new(&l).write_sidecar("IMG_1.JPG.tif").unwrap(), "IMG_1.JPG.tif.xmp");
    }

    #[test]
    fn names_match_case_insensitively() {
        let l = listing(&["img_1.cr3", "IMG_1.JPG", "Img_1.XMP"], &[]);
        let p = Plan::new(&l);
        assert_eq!(p.role("IMG_1.JPG"), paired("img_1.cr3"));
        assert_eq!(p.read_sidecar("img_1.cr3").as_deref(), Some("Img_1.XMP"));
        assert_eq!(p.write_sidecar("img_1.cr3").unwrap(), "Img_1.XMP");
        assert_eq!(p.move_set("img_1.cr3").unwrap(), vec!["IMG_1.JPG", "Img_1.XMP", "img_1.cr3"]);
    }

    #[test]
    fn darktable_style_sidecar_of_a_lone_raw() {
        // Read, edited in place (if it was not renamed) and moved with its photo.
        let l = listing(&["IMG_1.CR3", "IMG_1.CR3.xmp"], &[]);
        let p = Plan::new(&l);
        assert_eq!(p.read_sidecar("IMG_1.CR3").as_deref(), Some("IMG_1.CR3.xmp"));
        assert_eq!(p.write_sidecar("IMG_1.CR3").unwrap(), "IMG_1.CR3.xmp");
        assert_eq!(p.move_set("IMG_1.CR3").unwrap(), vec!["IMG_1.CR3", "IMG_1.CR3.xmp"]);
        assert_eq!(p.standard_rename("IMG_1.CR3"), Some(("IMG_1.CR3.xmp".into(), "IMG_1.xmp".into())));
    }

    #[test]
    fn standard_renames_only_when_nothing_can_mix() {
        let renames = |local: &[&str], partners: &[&str]| {
            let l = listing(local, partners);
            Plan::new(&l).standard_renames().into_iter().map(|(p, f, t)| format!("{p}: {f} -> {t}")).collect::<Vec<_>>()
        };
        // A lone RAW, a RAW+JPEG pair (digiKam style, any case) and a lone JPEG.
        assert_eq!(renames(&["IMG_1.CR3", "IMG_1.CR3.xmp"], &[]), vec!["IMG_1.CR3: IMG_1.CR3.xmp -> IMG_1.xmp"]);
        assert_eq!(
            renames(&["IMG_2.NEF", "IMG_2.JPG", "IMG_2.NEF.XMP", "IMG_2.JPG.xmp"], &[]),
            vec!["IMG_2.NEF: IMG_2.NEF.XMP -> IMG_2.xmp"]
        );
        assert_eq!(renames(&["IMG_3.jpg", "IMG_3.jpg.xmp"], &[]), vec!["IMG_3.jpg: IMG_3.jpg.xmp -> IMG_3.xmp"]);
        // The standard name is taken: both stay (the standard one is read).
        assert!(renames(&["IMG_1.CR3", "IMG_1.CR3.xmp", "IMG_1.xmp"], &[]).is_empty());
        // Another photo with the name, here or in the reject folder: never.
        assert!(renames(&["IMG_1.CR3", "IMG_1.TIF", "IMG_1.CR3.xmp", "IMG_1.TIF.xmp"], &[]).is_empty());
        assert!(renames(&["IMG_1.TIF", "IMG_1.TIF.xmp"], &["IMG_1.CR3"]).is_empty());
        assert!(renames(&["IMG_1.CR3", "IMG_1.CR3.xmp"], &["IMG_1.DNG"]).is_empty());
        // ... but a different photo with the identical name elsewhere is unrelated.
        assert_eq!(renames(&["IMG_1.CR3", "IMG_1.CR3.xmp"], &["IMG_1.CR3"]).len(), 1);
        // The JPEG half of a pair, and a name that is another photo's own sidecar: never.
        assert!(renames(&["IMG_1.CR3", "IMG_1.JPG", "IMG_1.JPG.xmp"], &[]).is_empty());
        assert!(renames(&["IMG_1.JPG", "IMG_1.JPG.tif", "IMG_1.JPG.tif.xmp"], &[]).is_empty());
    }

    #[test]
    fn reads_folder_and_partners() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("Trip");
        let rej = home.join("_Rejected");
        std::fs::create_dir_all(&rej).unwrap();
        std::fs::create_dir_all(home.join("Edits")).unwrap();
        for n in ["IMG_1.TIF", ".hidden.CR3"] {
            std::fs::write(home.join(n), n).unwrap();
        }
        for n in ["IMG_1.CR3", "IMG_1.xmp"] {
            std::fs::write(rej.join(n), n).unwrap();
        }
        std::fs::write(home.join("Edits/IMG_1.NEF"), "x").unwrap();
        let is_rej = |n: &str| n.eq_ignore_ascii_case("_rejected");
        let l = Listing::read(&home, false, &is_rej).unwrap();
        assert_eq!(l.local, vec!["IMG_1.TIF"]);
        let mut partners = l.partners.clone();
        partners.sort();
        assert_eq!(partners, vec!["IMG_1.CR3", "IMG_1.xmp"]);
        assert_eq!(Plan::new(&l).role("IMG_1.TIF"), Some(Role::Separate));
        let l = Listing::read(&rej, true, &is_rej).unwrap();
        assert_eq!(l.local, vec!["IMG_1.CR3", "IMG_1.xmp"]);
        assert_eq!(l.partners, vec!["IMG_1.TIF"]);
        assert_eq!(Plan::new(&l).role("IMG_1.CR3"), Some(Role::Owner));
    }
}
