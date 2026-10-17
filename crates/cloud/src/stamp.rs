//! Telling whether a document's file changed since WordCraft last read or wrote it.

use std::hash::Hasher;
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

/// Largest file [`DiskStamp::check`] reads to tell a real change from a sync app touching only
/// the file's date; a larger file whose size or date changed counts as changed.
pub const MAX_COMPARE_BYTES: u64 = 512 * 1024 * 1024;

/// The file as WordCraft last read or wrote it: its size, its modification time and a digest of
/// its bytes. The digest only has to tell bytes apart within one run of the app, so it is the
/// standard library's fast hash, not a cryptographic one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskStamp {
    len: u64,
    modified: Option<SystemTime>,
    digest: u64,
}

/// What [`DiskStamp::check`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskState {
    /// The same bytes are there (perhaps with a new date).
    Unchanged,
    /// Different bytes: another device or program saved the file.
    Changed,
    /// No file there any more: deleted, or moved or renamed elsewhere.
    Missing,
}

impl DiskState {
    /// The id commands report: `unchanged`, `changed` or `missing`.
    pub fn id(self) -> &'static str {
        match self {
            DiskState::Unchanged => "unchanged",
            DiskState::Changed => "changed",
            DiskState::Missing => "missing",
        }
    }
}

fn digest(bytes: &[u8]) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    h.write(bytes);
    h.finish()
}

impl DiskStamp {
    /// The stamp of `bytes`, just read from or written to `path`; the size and date are the
    /// file's as it is now.
    pub fn of_bytes(path: &Path, bytes: &[u8]) -> io::Result<DiskStamp> {
        let meta = std::fs::metadata(path)?;
        Ok(DiskStamp { len: meta.len(), modified: meta.modified().ok(), digest: digest(bytes) })
    }

    /// The stamp of the file at `path` as it is now (reads it, up to [`MAX_COMPARE_BYTES`]).
    pub fn read(path: &Path) -> io::Result<DiskStamp> {
        let bytes = read_capped(path)?;
        DiskStamp::of_bytes(path, &bytes)
    }

    /// Whether the file at `path` still holds what this stamp recorded. Only the size and date
    /// are looked at while they match; when they don't, the bytes are compared, and a file with
    /// the same bytes under a new date (a sync app touching it) is unchanged and the stamp takes
    /// the new date.
    pub fn check(&mut self, path: &Path) -> io::Result<DiskState> {
        let meta = match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => return Ok(DiskState::Missing),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(DiskState::Missing),
            Err(e) => return Err(e),
        };
        let modified = meta.modified().ok();
        if meta.len() == self.len && modified == self.modified {
            return Ok(DiskState::Unchanged);
        }
        if meta.len() != self.len || meta.len() > MAX_COMPARE_BYTES {
            return Ok(DiskState::Changed);
        }
        let bytes = match read_capped(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(DiskState::Missing),
            Err(e) => return Err(e),
        };
        if bytes.len() as u64 == self.len && digest(&bytes) == self.digest {
            self.modified = modified;
            Ok(DiskState::Unchanged)
        } else {
            Ok(DiskState::Changed)
        }
    }
}

fn read_capped(path: &Path) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_COMPARE_BYTES.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_COMPARE_BYTES {
        return Err(io::Error::other("file too large to compare"));
    }
    Ok(bytes)
}
