//! The only file-system operations Hyperfocal performs on the photo library besides
//! writing XMP sidecars: renaming a photo and its companion files into or out of the
//! reject folder.
//!
//! Safety rules, enforced here:
//! * Rename only (same file system). A failing rename is never turned into a
//!   copy-and-delete.
//! * Never overwrite: the destination is checked first and the rename itself uses
//!   `RENAME_NOREPLACE` where the file system supports it.
//! * All-or-nothing: if any file of a group fails to move, the files already moved
//!   are moved back.
//! * There is no delete function in this module (or anywhere for the library).

use anyhow::{Result, anyhow, bail};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::model::{RAW_EXTS, split_name};

/// rename(2) that refuses to replace an existing destination.
pub fn rename_noreplace(src: &Path, dst: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(dst).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", dst.display())));
    }
    let s = CString::new(src.as_os_str().as_bytes())?;
    let d = CString::new(dst.as_os_str().as_bytes())?;
    const RENAME_NOREPLACE: libc::c_uint = 1;
    // SAFETY: valid NUL-terminated paths; AT_FDCWD for relative resolution.
    let r = unsafe {
        libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, s.as_ptr(), libc::AT_FDCWD, d.as_ptr(), RENAME_NOREPLACE)
    };
    if r == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        // File systems without RENAME_NOREPLACE (some FUSE/SMB): plain rename after the
        // existence check above.
        Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP) => {
            if std::fs::symlink_metadata(dst).is_ok() {
                return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "destination exists"));
            }
            std::fs::rename(src, dst)
        }
        _ => Err(err),
    }
}

/// Files that belong to one photo: the file itself, its sidecars (`stem.xmp`,
/// `name.xmp`, `name.pp3`, ...) and same-stem non-RAW companions (the JPEG of a
/// RAW+JPEG pair). Other RAW files with the same stem are separate photos.
pub fn companions(dir: &Path, name: &str, stem: &str) -> Result<Vec<String>> {
    let stem_l = stem.to_lowercase();
    let name_l = name.to_lowercase();
    let mut out = Vec::new();
    for ent in std::fs::read_dir(dir)?.flatten() {
        let Some(n) = ent.file_name().to_str().map(|s| s.to_string()) else { continue };
        let Ok(ft) = ent.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        let l = n.to_lowercase();
        if l == name_l {
            out.push(n);
            continue;
        }
        if !l.starts_with(&format!("{stem_l}.")) {
            continue;
        }
        // Sidecars of this exact file (darktable/RawTherapee/DxO style) always belong.
        if l.starts_with(&format!("{name_l}.")) {
            out.push(n);
            continue;
        }
        let rest = &l[stem_l.len() + 1..];
        if rest.contains('.') {
            // e.g. IMG_1.CR3.xmp when we are IMG_1.DNG: belongs to the other file.
            continue;
        }
        let (_, ext) = split_name(&n);
        if RAW_EXTS.contains(&ext.as_str()) {
            continue; // another RAW with the same stem is its own photo
        }
        out.push(n);
    }
    if !out.iter().any(|n| n == name) {
        bail!("{name} is no longer in its folder");
    }
    out.sort();
    Ok(out)
}

/// Find an existing child directory matching `want` case-insensitively.
pub fn find_child_dir(dir: &Path, want: &str) -> Option<String> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let n = e.file_name().to_str()?.to_string();
        (e.file_type().ok()?.is_dir() && n.eq_ignore_ascii_case(want)).then_some(n)
    })
}

/// Move a group of files from `src_dir` to `dst_dir`, all or nothing.
pub fn move_group(src_dir: &Path, dst_dir: &Path, names: &[String]) -> Result<()> {
    for n in names {
        if std::fs::symlink_metadata(dst_dir.join(n)).is_ok() {
            bail!("{} already exists in {}; nothing was moved", n, dst_dir.display());
        }
        if n.contains('/') || n == ".." || n == "." {
            bail!("invalid file name {n}");
        }
    }
    let mut done: Vec<&String> = Vec::new();
    for n in names {
        let (s, d) = (src_dir.join(n), dst_dir.join(n));
        if let Err(e) = rename_noreplace(&s, &d) {
            // Put back what was already moved.
            let mut rollback_errors = Vec::new();
            for m in done.iter().rev() {
                if let Err(re) = rename_noreplace(&dst_dir.join(m), &src_dir.join(m)) {
                    rollback_errors.push(format!("{m}: {re}"));
                }
            }
            let hint = if e.raw_os_error() == Some(libc::EXDEV) {
                " (the destination is on another disk; Hyperfocal never copies-and-deletes)"
            } else {
                ""
            };
            if rollback_errors.is_empty() {
                return Err(anyhow!("moving {n} failed: {e}{hint}; nothing was moved"));
            }
            return Err(anyhow!(
                "moving {n} failed: {e}{hint}; could not move back: {}",
                rollback_errors.join(", ")
            ));
        }
        done.push(n);
    }
    Ok(())
}

pub fn ensure_dir(parent: &Path, name: &str) -> Result<PathBuf> {
    if let Some(existing) = find_child_dir(parent, name) {
        return Ok(parent.join(existing));
    }
    let p = parent.join(name);
    match std::fs::create_dir(&p) {
        Ok(()) => Ok(p),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(p),
        Err(e) => Err(anyhow!("creating {}: {e}", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_and_move() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        for n in ["IMG_1.CR3", "IMG_1.JPG", "IMG_1.xmp", "IMG_1.CR3.pp3", "IMG_1.DNG", "IMG_1.DNG.xmp", "IMG_10.CR3", "other.txt"] {
            std::fs::write(d.join(n), n).unwrap();
        }
        let g = companions(d, "IMG_1.CR3", "IMG_1").unwrap();
        assert_eq!(g, vec!["IMG_1.CR3", "IMG_1.CR3.pp3", "IMG_1.JPG", "IMG_1.xmp"]);
        let rej = ensure_dir(d, "_Rejected").unwrap();
        move_group(d, &rej, &g).unwrap();
        assert!(rej.join("IMG_1.CR3").exists());
        assert!(d.join("IMG_1.DNG").exists());
        // Contents survive the move unchanged.
        assert_eq!(std::fs::read_to_string(rej.join("IMG_1.JPG")).unwrap(), "IMG_1.JPG");
        // Never overwrites: a conflicting file blocks the whole group.
        std::fs::write(d.join("IMG_1.CR3"), "new").unwrap();
        std::fs::write(d.join("IMG_1.xmp"), "newer").unwrap();
        let g2 = vec!["IMG_1.CR3".to_string(), "IMG_1.xmp".to_string()];
        assert!(move_group(d, &rej, &g2).is_err());
        assert_eq!(std::fs::read_to_string(rej.join("IMG_1.CR3")).unwrap(), "IMG_1.CR3");
        assert_eq!(std::fs::read_to_string(d.join("IMG_1.CR3")).unwrap(), "new");
    }

    #[test]
    fn noreplace() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        assert!(rename_noreplace(&a, &b).is_err());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b");
    }
}
