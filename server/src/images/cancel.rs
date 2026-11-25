//! Stopping RAW work nobody waits for any more, with FlashRaw's cancel tokens.
//!
//! Blocking work runs under a token ([`scope`]). A FlashRaw decode started inside it stops
//! within a few milliseconds of [`CancelToken::cancel`] (FlashRaw checks between the strips,
//! tiles and bands it cuts the work into), the stages between decodes check it ([`check`]),
//! and the LibRaw helper process is killed (`native::run_with`). The token is kept per
//! thread, as FlashRaw keeps it, so nothing between a request and the decoder passes it on.
//! A token nobody cancels changes nothing: FlashRaw's output is bit-identical.

pub use flashraw::CancelToken;

/// Runs `f` with `token` as this thread's cancel token.
pub fn scope<R>(token: &CancelToken, f: impl FnOnce() -> R) -> R {
    flashraw_core::cancel::scope(Some(token), f)
}

/// The token of the work running on this thread, for `DecodeOptions::cancel`.
pub fn current() -> Option<CancelToken> {
    flashraw_core::cancel::current()
}

/// Whether the work running on this thread has been cancelled.
pub fn is_cancelled() -> bool {
    flashraw_core::cancel::is_cancelled()
}

/// `Err(Cancelled)` once the work running on this thread has been cancelled.
pub fn check() -> anyhow::Result<()> {
    if is_cancelled() { Err(Cancelled.into()) } else { Ok(()) }
}

/// Reads a whole file a few megabytes at a time, stopping between them once the work is
/// cancelled: a large RAW file on a spinning array disk takes a while to read.
pub fn read(path: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    // Debug mode: how long reading originals takes (array disks spinning up show here).
    let mut t = crate::tel_op!("raw.read", 1000);
    t.detail(|| path.display().to_string());
    let mut f = std::fs::File::open(path)?;
    let mut data = Vec::with_capacity(f.metadata().map(|m| m.len() as usize).unwrap_or(0));
    loop {
        check()?;
        if (&mut f).take(4 << 20).read_to_end(&mut data)? == 0 {
            crate::tel_count!("raw.read_bytes", data.len());
            return Ok(data);
        }
    }
}

/// The work was stopped because its result was no longer wanted. Nothing is wrong with
/// the file: it must not be recorded as unreadable, nor handed to another decoder.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("cancelled")]
pub struct Cancelled;

/// Ties blocking work to the request waiting for it: cancels the token when dropped, as
/// a request is when the browser gives up on it (at its `.await`).
#[derive(Default)]
pub struct CancelOnDrop(CancelToken);

impl CancelOnDrop {
    pub fn token(&self) -> CancelToken {
        self.0.clone()
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_to_the_thread() {
        assert!(current().is_none() && check().is_ok());
        let t = CancelToken::new();
        scope(&t, || {
            assert!(check().is_ok());
            t.cancel();
            assert!(check().unwrap_err().is::<Cancelled>());
            // Other threads have their own (none).
            assert!(std::thread::spawn(|| !is_cancelled()).join().unwrap());
        });
        assert!(!is_cancelled());
    }

    #[test]
    fn reads_whole_files_unless_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let data: Vec<u8> = (0..9 << 20).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        assert!(read(&p).unwrap() == data);
        let t = CancelToken::new();
        t.cancel();
        assert!(scope(&t, || read(&p)).unwrap_err().is::<Cancelled>());
    }
}
