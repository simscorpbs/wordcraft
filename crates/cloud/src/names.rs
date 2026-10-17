//! File names for a new document saved to the cloud and for the copy kept in a conflict.

use std::path::{Path, PathBuf};

/// How many numbered names are tried before giving up.
const MAX_TRIES: u32 = 10_000;

/// A file in `dir` named `stem.ext` that doesn't exist yet: `stem.ext`, else `stem 2.ext`,
/// `stem 3.ext`… as Finder and File Explorer number copies. Characters a file name can't hold
/// on any system are replaced by `-`, and a blank stem becomes `Document`. `None` only when ten
/// thousand names are all taken.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> Option<PathBuf> {
    let stem = clean_stem(stem);
    let ext = ext.trim_start_matches('.');
    (1..=MAX_TRIES).map(|n| dir.join(numbered(&stem, n, ext))).find(|p| !p.exists() && !is_icloud_placeholder_for(p))
}

/// Where to keep this computer's version of `path` when another device changed the file:
/// beside it, as `Name (tag).ext`, numbered like [`unique_path`] when that is taken.
pub fn conflict_copy_path(path: &Path, tag: &str) -> Option<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new(""));
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    unique_path(dir, &format!("{} ({})", clean_stem(&stem), clean_stem(tag)), &ext)
}

fn numbered(stem: &str, n: u32, ext: &str) -> String {
    let stem = if n == 1 { stem.to_string() } else { format!("{stem} {n}") };
    if ext.is_empty() { stem } else { format!("{stem}.{ext}") }
}

fn clean_stem(stem: &str) -> String {
    let cleaned: String = stem
        .chars()
        .map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c })
        .take(200)
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    if cleaned.is_empty() { "Document".to_string() } else { cleaned.to_string() }
}

/// A name iCloud Drive already uses for a file that is in the cloud only counts as taken.
fn is_icloud_placeholder_for(path: &Path) -> bool {
    path.file_name().is_some_and(|name| path.with_file_name(format!(".{}.icloud", name.to_string_lossy())).exists())
}
