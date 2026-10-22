//! Local version history for desktop documents.
//!
//! The service backed histories of iCloud Drive, Google Drive and OneDrive remain owned by
//! those services.  This module is the small local safety net WordCraft keeps in the profile:
//! document bytes are written by a worker, beside a bounded JSON index.
//!
//! Storage, per document, under `<root>/<key>/` where the key hashes the canonical path:
//! - `<id>.parts.json` and `objects/<sha256>`: a version of a zip package (`.docx`, `.odt`…)
//!   kept as its parts, each part an object named by the hash of its content, so what did not
//!   change between versions is stored once and what changed is a small delta (see [`pack`]);
//! - `<id>.bin`: the exact bytes of one version, for the `original` copy, files that are not a
//!   zip, and versions written by earlier builds; verified after writing;
//! - `index.json`: the versions' metadata ([`VersionInfo`]), newest first;
//! - `document.json`: the path the bucket last belonged to, so a file moved outside WordCraft
//!   can find its history again (see `adopt_moved`).
//!
//! Safety rules: before any save writes over a file whose current bytes are not in the history
//! (a file that never had history, or one changed by another program), those bytes are stored
//! first as an `original` version ([`crate::Session::protect_before_overwrite`]). A failure of the
//! history never fails or alters the save itself: it is reported in the store's `error`.

use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::collections::BTreeSet;
#[cfg(not(target_arch = "wasm32"))]
use std::fs::{self, File, OpenOptions};
#[cfg(not(target_arch = "wasm32"))]
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Mutex, OnceLock, mpsc};

/// A version's origin.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VersionKind {
    #[default]
    Save,
    Autosave,
    Restore,
    /// The bytes that were on disk before WordCraft first wrote over them: a file that had no
    /// history yet (a `.docx` made before WordCraft), or one another program changed.
    Original,
}

impl VersionKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::Autosave => "autosave",
            Self::Restore => "restore",
            Self::Original => "original",
        }
    }
}

/// How a version's bytes are kept on disk.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VersionStorage {
    /// The bytes as they were saved, in `<id>.bin`.
    #[default]
    Whole,
    /// A zip package split into content-addressed parts, in `<id>.parts.json` (see [`pack`]).
    Parts,
}

fn is_whole(storage: &VersionStorage) -> bool {
    *storage == VersionStorage::Whole
}

/// The metadata shown by `file.versions` and the Version History pane.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub id: String,
    pub saved_at: String,
    pub author: String,
    pub size: u64,
    pub kind: VersionKind,
    /// AutoSave versions only: when the ten-minute window this version gathers began. Later
    /// AutoSave writes replace the version until the window closes, so a long editing session
    /// still leaves one version every ten minutes instead of a single sliding one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_start: Option<String>,
    /// SHA-256 of the stored bytes (lower-case hex). Checked when the version is read back, and
    /// used to tell whether the file on disk is already in the history. Older entries lack it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// AutoSave versions only: the editing session (one opening of the document in one run of
    /// the app) that wrote it. AutoSave joins only a window its own session opened, so the first
    /// change after opening a document always starts a new version, as Word's AutoSave does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// `original` versions only: when WordCraft stored the copy. `savedAt` is the file's own
    /// modification time; retention counts from this capture time, so an old file's original
    /// is not pruned the moment it is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
    /// How the bytes are kept. `size` stays the size of the document as it was saved.
    #[serde(default, skip_serializing_if = "is_whole")]
    pub storage: VersionStorage,
    /// Disk space this version added when it was stored: its manifest and the parts no earlier
    /// version had, or its whole bytes. Absent for versions written by earlier builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored: Option<u64>,
}

/// Format a stored UTC timestamp for the history pane.  Version metadata stays UTC on disk so
/// pruning and clients have a stable value; the pane converts it to the computer's local clock.
/// A missing or unreadable zone database falls back to UTC, which is still an unambiguous time.
pub fn local_timestamp(saved_at: &str) -> String {
    let Some(secs) = parse_iso_timestamp(saved_at) else { return saved_at.to_owned() };
    #[cfg(not(target_arch = "wasm32"))]
    let offset = local_offset(secs);
    #[cfg(target_arch = "wasm32")]
    let offset = 0i32;
    let Some(local) = i64::try_from(secs).ok().and_then(|value| value.checked_add(i64::from(offset))) else {
        return saved_at.to_owned();
    };
    let days = local.div_euclid(86_400);
    let seconds = local.rem_euclid(86_400);
    let (year, month, day) = crate::cmd::civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}", seconds / 3_600, seconds / 60 % 60)
}

fn parse_iso_timestamp(value: &str) -> Option<u64> {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || bytes.get(19) != Some(&b'Z')
    {
        return None;
    }
    let year = i64::try_from(parse_decimal(bytes.get(0..4)?)?).ok()?;
    let month = parse_decimal(bytes.get(5..7)?)?;
    let day = parse_decimal(bytes.get(8..10)?)?;
    let hour = parse_decimal(bytes.get(11..13)?)?;
    let minute = parse_decimal(bytes.get(14..16)?)?;
    let second = parse_decimal(bytes.get(17..19)?)?;
    if !(1..=12).contains(&month) || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let month = i64::try_from(month).ok()?;
    let day = i64::try_from(day).ok()?;
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era.checked_mul(146_097)?.checked_add(doe)?.checked_sub(719_468)?;
    if days < 0 {
        return None;
    }
    u64::try_from(days).ok()?.checked_mul(86_400)?.checked_add(hour.checked_mul(3_600)?)?.checked_add(minute.checked_mul(60)?)?.checked_add(second)
}

fn parse_decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0u64;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
    }
    Some(value)
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct LocalZone {
    default_offset: i32,
    transitions: Vec<(i64, i32)>,
}

#[cfg(not(target_arch = "wasm32"))]
impl LocalZone {
    fn offset_at(&self, secs: i64) -> i32 {
        match self.transitions.binary_search_by_key(&secs, |(at, _)| *at) {
            Ok(index) => self.transitions.get(index).map(|(_, offset)| *offset).unwrap_or(self.default_offset),
            Err(0) => self.default_offset,
            Err(index) => self.transitions.get(index.saturating_sub(1)).map(|(_, offset)| *offset).unwrap_or(self.default_offset),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn local_offset(secs: u64) -> i32 {
    static ZONE: OnceLock<Option<LocalZone>> = OnceLock::new();
    let Ok(secs) = i64::try_from(secs) else { return 0 };
    ZONE.get_or_init(load_local_zone).as_ref().map_or(0, |zone| zone.offset_at(secs))
}

#[cfg(not(target_arch = "wasm32"))]
fn load_local_zone() -> Option<LocalZone> {
    #[cfg(windows)]
    {
        // The standard library has no local-time conversion API on Windows.  PowerShell is part
        // of every supported desktop installation, and this is run once per process (the result
        // is held by `OnceLock`), so the history pane does not start a process for every row.
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "[DateTimeOffset]::Now.Offset.TotalSeconds"])
            .output()
            .ok()?;
        if !output.status.success() || output.stdout.len() > 64 {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let seconds = text.trim().parse::<i32>().ok()?;
        if seconds.unsigned_abs() > 86_400 {
            return None;
        }
        Some(LocalZone { default_offset: seconds, transitions: Vec::new() })
    }
    #[cfg(not(windows))]
    {
        let path = std::env::var_os("TZ")
            .filter(|value| value.as_encoded_bytes().first() == Some(&b'/'))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/etc/localtime"));
        let metadata = std::fs::metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() > 1 << 20 {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        parse_tzif(&bytes)
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(windows)))]
fn parse_tzif(bytes: &[u8]) -> Option<LocalZone> {
    let (version, counts) = tzif_header(bytes, 0)?;
    let (offset, time_size) = if matches!(version, b'2' | b'3' | b'4') {
        let first_size = tzif_block_size(counts, 4)?;
        let second = 44usize.checked_add(first_size)?;
        (second, 8)
    } else {
        (0, 4)
    };
    let (_, counts) = tzif_header(bytes, offset)?;
    let (timecnt, typecnt) = (counts.3, counts.4);
    if typecnt == 0 || typecnt > 256 || timecnt > 200_000 {
        return None;
    }
    let mut cursor = offset.checked_add(44)?;
    let mut transition_times = Vec::with_capacity(timecnt);
    for _ in 0..timecnt {
        let end = cursor.checked_add(time_size)?;
        let raw = bytes.get(cursor..end)?;
        let at = if time_size == 8 { i64::from_be_bytes(raw.try_into().ok()?) } else { i64::from(i32::from_be_bytes(raw.try_into().ok()?)) };
        transition_times.push(at);
        cursor = end;
    }
    let transition_types = bytes.get(cursor..cursor.checked_add(timecnt)?)?;
    cursor = cursor.checked_add(timecnt)?;
    let mut offsets = Vec::with_capacity(typecnt);
    for _ in 0..typecnt {
        let end = cursor.checked_add(6)?;
        let info = bytes.get(cursor..end)?;
        offsets.push(i32::from_be_bytes(info.get(..4)?.try_into().ok()?));
        cursor = end;
    }
    let default_offset = *offsets.first()?;
    let mut transitions = Vec::with_capacity(timecnt);
    for (at, kind) in transition_times.into_iter().zip(transition_types) {
        let offset = offsets.get(usize::from(*kind)).copied()?;
        transitions.push((at, offset));
    }
    Some(LocalZone { default_offset, transitions })
}

#[cfg(all(not(target_arch = "wasm32"), not(windows)))]
fn tzif_header(bytes: &[u8], offset: usize) -> Option<(u8, (usize, usize, usize, usize, usize, usize))> {
    let header = bytes.get(offset..offset.checked_add(44)?)?;
    if header.get(..4)? != b"TZif" {
        return None;
    }
    let version = *header.get(4)?;
    let mut counts = [0usize; 6];
    for (index, count) in counts.iter_mut().enumerate() {
        let start = 20usize.checked_add(index.checked_mul(4)?)?;
        let raw = header.get(start..start.checked_add(4)?)?;
        *count = usize::try_from(u32::from_be_bytes(raw.try_into().ok()?)).ok()?;
    }
    Some((version, (counts[0], counts[1], counts[2], counts[3], counts[4], counts[5])))
}

#[cfg(all(not(target_arch = "wasm32"), not(windows)))]
fn tzif_block_size(counts: (usize, usize, usize, usize, usize, usize), time_size: usize) -> Option<usize> {
    let (leapcnt, timecnt, typecnt, charcnt) = (counts.2, counts.3, counts.4, counts.5);
    timecnt
        .checked_mul(time_size)?
        .checked_add(timecnt)?
        .checked_add(typecnt.checked_mul(6)?)?
        .checked_add(charcnt)?
        .checked_add(leapcnt.checked_mul(time_size.checked_add(4)?)?)?
        .checked_add(counts.0)?
        .checked_add(counts.1)
}

/// The source document retained while a previous version is being viewed.
#[derive(Clone, Debug)]
pub struct OpenedVersion {
    pub(crate) path: PathBuf,
    pub(crate) info: VersionInfo,
    /// The document that was active before this previous version was opened.  Keeping it here
    /// lets Restore replace the original document in one undoable step instead of losing the
    /// unsaved work when the read-only copy is shown.
    pub(crate) original_doc: wordcraft_doc::Document,
    pub(crate) original_sel: crate::Selection,
    /// What else opening the version replaced, so closing it (`file.closeVersion`) or restoring
    /// it gives back the same document: whether it had changes not yet written to its file
    /// (Restore stores those as a version first, so the state before the restore stays in the
    /// history), its password, and the disk stamp and conflict AutoSave checks before it writes.
    pub(crate) original_dirty: bool,
    pub(crate) original_password: Option<crate::io::Password>,
    pub(crate) original_stamp: Option<wordcraft_cloud::DiskStamp>,
    pub(crate) original_conflict: Option<wordcraft_cloud::DiskState>,
    /// The document's AutoSave switch: opening the version (a document with no file) set the
    /// default, and closing it gives the document its own switch back.
    pub(crate) original_autosave: bool,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod pack;

#[cfg(not(target_arch = "wasm32"))]
const INDEX_FILE: &str = "index.json";
#[cfg(not(target_arch = "wasm32"))]
const PARTS_SUFFIX: &str = ".parts.json";
#[cfg(not(target_arch = "wasm32"))]
const OBJECTS_DIR: &str = "objects";
#[cfg(not(target_arch = "wasm32"))]
const MAX_MANIFEST_BYTES: u64 = 8 << 20;
/// Largest object file read: a whole part compressed (incompressible data grows a little).
#[cfg(not(target_arch = "wasm32"))]
const MAX_OBJECT_FILE: u64 = pack::MAX_PART_BYTES + (16 << 20);
#[cfg(not(target_arch = "wasm32"))]
const META_FILE: &str = "document.json";
#[cfg(not(target_arch = "wasm32"))]
const MAX_INDEX_BYTES: u64 = 8 << 20;
#[cfg(not(target_arch = "wasm32"))]
const MAX_META_BYTES: u64 = 64 << 10;
/// A single document is never allowed to make the profile larger than 500 MiB, counted as the
/// space its history really takes on disk (shared parts once). This is WordCraft's own disk
/// budget: OneDrive and SharePoint count versions against the account's storage instead, which
/// a local profile does not have. A single saved document may not be larger either.
pub const MAX_DOCUMENT_BYTES: u64 = 500 << 20;
/// At most 500 versions per document: the count limit of SharePoint's and OneDrive for work's
/// automatic version limits (Microsoft Learn, "Plan version storage for document libraries").
/// The pinned original (see [`prune`]) is counted in this limit.
pub const MAX_VERSIONS: usize = 500;
/// AutoSave writes inside one window update a single version. Word's AutoSave on OneDrive adds a
/// version at the first change after the document is opened and then only about every ten
/// minutes for the rest of the editing session (Microsoft Support, "What is AutoSave?").
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const AUTOSAVE_WINDOW_SECS: u64 = 10 * 60;
// Retention follows the automatic version limits of SharePoint and OneDrive for work (Microsoft
// Learn, "Plan version storage for document libraries"): every version for the first 30 days,
// then one per hour up to 60 days, one per day up to 180 days, and one per week after that, for
// as long as the count limit allows. Age alone therefore never empties a history: a document
// left untouched keeps its weekly versions. (A personal OneDrive keeps fewer, the last 25
// versions; WordCraft keeps the larger schedule because this copy is local and private.)
// Microsoft describes the kept versions as those at the start of each hour, day or week;
// WordCraft keeps the newest version of each period, the state the document was left in.
#[cfg(not(target_arch = "wasm32"))]
const DAY_SECS: u64 = 24 * 60 * 60;
/// Every version younger than this is kept.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const KEEP_ALL_SECS: u64 = 30 * DAY_SECS;
/// Up to this age, one version per hour is kept.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const KEEP_HOURLY_SECS: u64 = 60 * DAY_SECS;
/// Up to this age, one version per day is kept; older versions are kept one per week.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const KEEP_DAILY_SECS: u64 = 180 * DAY_SECS;
#[cfg(not(target_arch = "wasm32"))]
const MAX_ID_LEN: usize = 32;
/// Most per-document folders looked at when a file with no history may have been moved.
#[cfg(not(target_arch = "wasm32"))]
const MAX_BUCKET_SCAN: usize = 20_000;
/// Most version files recovered from a folder whose index is missing or unreadable.
#[cfg(not(target_arch = "wasm32"))]
const MAX_RECOVERED_FILES: usize = 10_000;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct VersionWrite {
    path: PathBuf,
    bytes: Vec<u8>,
    author: String,
    kind: VersionKind,
    saved_at: String,
    /// The editing session of an AutoSave write; `None` never joins an existing version.
    session: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
enum Job {
    Save(VersionWrite),
    /// List the versions of a path. With `probe`, a path without history may look for the
    /// history of a file moved outside WordCraft (it reads the file to compare its bytes).
    List {
        path: PathBuf,
        probe: bool,
    },
    /// `file.rename` moved the document: its history follows it.
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
}

#[cfg(not(target_arch = "wasm32"))]
struct Event {
    path: PathBuf,
    list: Vec<VersionInfo>,
    result: Result<(), String>,
}

/// The bucket's identity record.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize, Serialize)]
struct DocumentMeta {
    path: String,
}

/// Desktop worker state.  Nothing in this struct is shared with the worker by reference, so a
/// slow disk never holds a borrow of the editing session.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Versions {
    tx: mpsc::SyncSender<Job>,
    rx: mpsc::Receiver<Event>,
    root: PathBuf,
    /// Held by the worker for each job and by the few synchronous writes (the original copy
    /// before an overwrite), so two writers never interleave in one index.
    lock: Arc<Mutex<()>>,
    /// Random per run of the app: with the session's document id it names an editing session.
    run_id: String,
    /// `(document id, canonical path)` whose bytes on disk are known to be in the history for
    /// the document open now. Reset by opening another document (its id changes).
    verified: Option<(u64, PathBuf)>,
    pub list: Vec<VersionInfo>,
    pub ready: bool,
    pub busy: bool,
    pub error: Option<String>,
    current_path: Option<PathBuf>,
    alive: bool,
}

#[cfg(not(target_arch = "wasm32"))]
fn private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.is_dir() { Ok(()) } else { Err(io::Error::new(io::ErrorKind::AlreadyExists, "version history path is not a directory")) }
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn private_file(path: &Path, truncate: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(not(target_arch = "wasm32"))]
fn id_ok(id: &str) -> bool {
    id.len() == MAX_ID_LEN && id.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(not(target_arch = "wasm32"))]
fn key_ok(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(not(target_arch = "wasm32"))]
fn new_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// SHA-256 of `bytes` as lower-case hex: a digest that stays the same across runs and builds,
/// unlike the standard library's hasher.
#[cfg(not(target_arch = "wasm32"))]
fn digest_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Canonical paths are stable across runs.  A file that does not exist yet uses its canonical
/// parent and its own name, which keeps a Save As target in the same history bucket after the
/// first write.
#[cfg(not(target_arch = "wasm32"))]
pub fn canonical_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    let file = path.file_name().map(PathBuf::from);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    match file {
        Some(file) => parent.join(file),
        None => parent,
    }
}

#[cfg(target_arch = "wasm32")]
pub fn canonical_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// FNV-1a of `text`, as 16 hex digits: a stable folder name for a document's history.
fn fnv_key(text: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// The history folder name of the document at `path`: a hash of its canonical path. Windows
/// file names ignore case, so there the path is lower-cased first and `Report.docx` and
/// `report.docx` share one history whichever spelling reaches the engine.
pub fn document_key(path: &Path) -> String {
    let canonical = canonical_path(path);
    let text = canonical.to_string_lossy();
    if cfg!(windows) { fnv_key(&text.to_lowercase()) } else { fnv_key(&text) }
}

/// The folder name earlier builds used: the canonical path with its case as given.
#[cfg(not(target_arch = "wasm32"))]
fn legacy_key(path: &Path) -> String {
    fnv_key(&canonical_path(path).to_string_lossy())
}

/// The document's history folder. A folder an earlier build made under the case-sensitive key
/// is moved to the current key the first time it is looked up; if the move fails it is used
/// where it is, so nothing is hidden.
#[cfg(not(target_arch = "wasm32"))]
fn document_dir(root: &Path, path: &Path) -> PathBuf {
    let dir = root.join(document_key(path));
    if !cfg!(windows) || fs::symlink_metadata(&dir).is_ok() {
        return dir;
    }
    let legacy = root.join(legacy_key(path));
    if legacy == dir || !fs::symlink_metadata(&legacy).is_ok_and(|m| m.is_dir()) {
        return dir;
    }
    if fs::rename(&legacy, &dir).is_ok() || fs::symlink_metadata(&dir).is_ok() { dir } else { legacy }
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_number(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    let mut n = 0u64;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
    }
    Some(n)
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_iso_secs(value: &str) -> Option<u64> {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || bytes.get(19) != Some(&b'Z')
    {
        return None;
    }
    let year = i64::try_from(parse_number(bytes.get(0..4)?)?).ok()?;
    let month = parse_number(bytes.get(5..7)?)?;
    let day = parse_number(bytes.get(8..10)?)?;
    let hour = parse_number(bytes.get(11..13)?)?;
    let minute = parse_number(bytes.get(14..16)?)?;
    let second = parse_number(bytes.get(17..19)?)?;
    if !(1..=12).contains(&month) || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let month = i64::try_from(month).ok()?;
    let day = i64::try_from(day).ok()?;
    // Howard Hinnant's civil-date conversion, inverse of cmd::civil_from_days.
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era.checked_mul(146097)?.checked_add(doe)?.checked_sub(719468)?;
    if days < 0 {
        return None;
    }
    let days = u64::try_from(days).ok()?;
    days.checked_mul(86_400)?.checked_add(hour.checked_mul(3_600)?)?.checked_add(minute.checked_mul(60)?)?.checked_add(second)
}

#[cfg(not(target_arch = "wasm32"))]
fn modified_secs(meta: &fs::Metadata) -> Option<u64> {
    meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Newest first. The sort is stable and new entries are inserted at the front, so versions
/// saved within the same second keep the order they were written in.
#[cfg(not(target_arch = "wasm32"))]
fn sort_newest_first(list: &mut [VersionInfo]) {
    list.sort_by_key(|v| std::cmp::Reverse(parse_iso_secs(&v.saved_at).unwrap_or(0)));
}

/// Read a small regular file, refusing anything larger than `max` or not a plain file.
#[cfg(not(target_arch = "wasm32"))]
fn read_small(path: &Path, max: u64) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > max {
        return None;
    }
    Some(bytes)
}

/// The versions recorded in `dir`, newest first. Hostile or damaged indexes never cost the
/// stored bytes: entries that do not parse are skipped one by one, and a version file the index
/// does not mention (an index lost or corrupted, or a write interrupted between the version and
/// the index) comes back as a `save` entry dated by its file, so pruning never deletes it just
/// because the index forgot it.
#[cfg(not(target_arch = "wasm32"))]
fn read_index(dir: &Path) -> Vec<VersionInfo> {
    let Ok(dir_meta) = fs::symlink_metadata(dir) else { return Vec::new() };
    if !dir_meta.is_dir() {
        return Vec::new();
    }
    let mut list: Vec<VersionInfo> = read_small(&dir.join(INDEX_FILE), MAX_INDEX_BYTES)
        .and_then(|bytes| serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| serde_json::from_value::<VersionInfo>(value).ok())
        .collect();
    let mut seen = BTreeSet::new();
    list.retain(|v| {
        id_ok(&v.id)
            && !seen.contains(&v.id)
            && v.size <= MAX_DOCUMENT_BYTES
            && parse_iso_secs(&v.saved_at).is_some()
            && match v.storage {
                VersionStorage::Whole => fs::symlink_metadata(dir.join(format!("{}.bin", v.id))).is_ok_and(|m| m.is_file() && m.len() == v.size),
                VersionStorage::Parts => fs::symlink_metadata(manifest_path(dir, &v.id)).is_ok_and(|m| m.is_file()),
            }
            && seen.insert(v.id.clone())
    });
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten().take(MAX_RECOVERED_FILES) {
            let name = entry.file_name();
            if let Some(id) = name.to_str().and_then(|n| n.strip_suffix(PARTS_SUFFIX))
                && id_ok(id)
                && !seen.contains(id)
                && let Some(manifest) = read_manifest(dir, id)
            {
                // A manifest carries its version's metadata, so it comes back as it was.
                seen.insert(id.to_owned());
                list.push(VersionInfo { storage: VersionStorage::Parts, ..manifest.info });
                continue;
            }
            let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".bin")) else { continue };
            if !id_ok(id) || seen.contains(id) {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
            if !meta.is_file() || meta.len() > MAX_DOCUMENT_BYTES {
                continue;
            }
            seen.insert(id.to_owned());
            list.push(VersionInfo {
                id: id.to_owned(),
                saved_at: crate::cmd::iso_from_unix_secs(modified_secs(&meta).unwrap_or(0)),
                author: String::new(),
                size: meta.len(),
                kind: VersionKind::Save,
                ..VersionInfo::default()
            });
        }
    }
    sort_newest_first(&mut list);
    list
}

#[cfg(not(target_arch = "wasm32"))]
fn read_meta(dir: &Path) -> Option<DocumentMeta> {
    let bytes = read_small(&dir.join(META_FILE), MAX_META_BYTES)?;
    serde_json::from_slice(&bytes).ok()
}

/// Record which file the bucket belongs to now (only when it changed).
#[cfg(not(target_arch = "wasm32"))]
fn write_meta(dir: &Path, path: &Path) -> Result<(), String> {
    let Some(text) = path.to_str() else { return Ok(()) };
    if read_meta(dir).is_some_and(|meta| meta.path == text) {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&DocumentMeta { path: text.to_owned() }).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(META_FILE), &bytes)
}

/// Write `bytes` to `path` through a temporary file: the bytes are flushed, read back and
/// compared before the temporary file replaces `path`, so a short or damaged write never
/// replaces good data and never enters the history.
#[cfg(not(target_arch = "wasm32"))]
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    private_dir(parent).map_err(|e| e.to_string())?;
    let id = new_id()?;
    let tmp = parent.join(format!(".{}.{}.tmp", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(), id));
    let result = (|| {
        let mut file = private_file(&tmp, true).map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        let limit = u64::try_from(bytes.len()).map_err(|e| e.to_string())?;
        let mut written = Vec::with_capacity(bytes.len());
        File::open(&tmp).map_err(|e| e.to_string())?.take(limit.saturating_add(1)).read_to_end(&mut written).map_err(|e| e.to_string())?;
        if written != bytes {
            return Err("version history write could not be verified".to_string());
        }
        drop(written);
        match fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(first) if first.kind() == io::ErrorKind::AlreadyExists => {
                // Unix rename replaces an existing destination; Windows does not. Move the old
                // file aside so the prepared bytes can still be committed without truncating it.
                let backup = parent.join(format!(".{}.{}.bak", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(), new_id()?));
                fs::rename(path, &backup).map_err(|error| format!("{first}; {error}"))?;
                match fs::rename(&tmp, path) {
                    Ok(()) => {
                        let _ = fs::remove_file(&backup);
                        Ok(())
                    }
                    Err(error) => {
                        let _ = fs::rename(&backup, path);
                        Err(error.to_string())
                    }
                }
            }
            Err(error) => Err(error.to_string()),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(not(target_arch = "wasm32"))]
fn save_index(dir: &Path, list: &[VersionInfo]) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(INDEX_FILE), &bytes)
}

#[cfg(not(target_arch = "wasm32"))]
fn write_version_file(dir: &Path, id: &str, bytes: &[u8]) -> Result<(), String> {
    write_atomic(&dir.join(format!("{id}.bin")), bytes)
}

#[cfg(not(target_arch = "wasm32"))]
fn manifest_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}{PARTS_SUFFIX}"))
}

#[cfg(not(target_arch = "wasm32"))]
fn object_path(dir: &Path, hash: &str) -> PathBuf {
    dir.join(OBJECTS_DIR).join(hash)
}

/// The manifest of version `id`, checked before anything is built from it.
#[cfg(not(target_arch = "wasm32"))]
fn read_manifest(dir: &Path, id: &str) -> Option<pack::Manifest> {
    let bytes = read_small(&manifest_path(dir, id), MAX_MANIFEST_BYTES)?;
    let manifest: pack::Manifest = serde_json::from_slice(&bytes).ok()?;
    (pack::manifest_ok(&manifest) && manifest.info.id == id).then_some(manifest)
}

/// The header of an object file, and the file's size.
#[cfg(not(target_arch = "wasm32"))]
fn object_header(dir: &Path, hash: &str) -> Result<(pack::ObjectHeader, u64), String> {
    let path = object_path(dir, hash);
    let meta = fs::symlink_metadata(&path).map_err(|_| "a part of this version is missing".to_string())?;
    if !meta.is_file() {
        return Err("a part of this version is missing".into());
    }
    let mut head = Vec::with_capacity(pack::HEADER_MAX);
    File::open(&path).map_err(|e| e.to_string())?.take(pack::HEADER_MAX as u64).read_to_end(&mut head).map_err(|e| e.to_string())?;
    Ok((pack::ObjectHeader::parse(&head)?, meta.len()))
}

/// The content of object `hash`: its chain of deltas is followed back to a whole copy, and
/// every step is checked against its own name, so a damaged object anywhere in the chain is an
/// error for the versions that use it and nothing else.
#[cfg(not(target_arch = "wasm32"))]
fn load_object(dir: &Path, hash: &str) -> Result<Vec<u8>, String> {
    let mut chain: Vec<(String, Vec<u8>)> = Vec::new();
    let mut current = hash.to_owned();
    loop {
        if !pack::hash_ok(&current) || chain.len() >= pack::MAX_CHAIN {
            return Err("a part of this version is damaged".into());
        }
        let bytes = read_small(&object_path(dir, &current), MAX_OBJECT_FILE).ok_or("a part of this version is missing")?;
        let base = pack::ObjectHeader::parse(&bytes)?.base;
        chain.push((current, bytes));
        match base {
            Some(base) => current = base,
            None => break,
        }
    }
    let mut content: Option<Vec<u8>> = None;
    for (name, bytes) in chain.iter().rev() {
        let next = pack::decode(bytes, content.as_deref())?;
        if digest_hex(&next) != *name {
            return Err("a part of this version is damaged (its checksum does not match)".into());
        }
        content = Some(next);
    }
    content.ok_or_else(|| "a part of this version is missing".into())
}

/// Store the parts of a version as objects, each a delta against the same part of `base` when
/// that pays. An object already there is reused only when it still reads back correctly; a
/// damaged one is written again whole, which also repairs every version that uses it. Returns
/// the bytes newly written.
#[cfg(not(target_arch = "wasm32"))]
fn store_parts(dir: &Path, parts: &[pack::Part], base: Option<&pack::Manifest>) -> Result<u64, String> {
    private_dir(&dir.join(OBJECTS_DIR)).map_err(|e| e.to_string())?;
    let mut written = 0u64;
    for part in parts.iter().filter(|part| !part.meta.dir) {
        let hash = part.meta.sha256.as_str();
        let path = object_path(dir, hash);
        let object = if fs::symlink_metadata(&path).is_ok() {
            if load_object(dir, hash).is_ok() {
                continue;
            }
            pack::encode_full(&part.data)
        } else {
            let base_part = base.and_then(|manifest| manifest.parts.iter().find(|p| !p.dir && p.name == part.meta.name && p.sha256 != hash));
            let base = base_part.and_then(|p| {
                let depth = object_header(dir, &p.sha256).ok()?.0.depth;
                Some((p.sha256.clone(), depth, load_object(dir, &p.sha256).ok()?))
            });
            pack::encode(&part.data, base.as_ref().map(|(hash, depth, content)| (hash.as_str(), *depth, content.as_slice())))
        };
        write_atomic(&path, &object)?;
        if let Err(error) = load_object(dir, hash) {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        written = written.saturating_add(object.len() as u64);
    }
    Ok(written)
}

/// The newest version kept as parts whose manifest reads: the base for the next deltas.
#[cfg(not(target_arch = "wasm32"))]
fn newest_manifest(dir: &Path, list: &[VersionInfo]) -> Option<pack::Manifest> {
    list.iter().filter(|v| v.storage == VersionStorage::Parts).find_map(|v| read_manifest(dir, &v.id))
}

/// Keep `bytes` (a zip package) as parts for version `info`: objects first, then the manifest.
/// The archive is rebuilt in memory and split again before anything is written, so a version
/// stored as parts is known to come back with the same parts. Returns the space it added.
#[cfg(not(target_arch = "wasm32"))]
fn write_parts_version(dir: &Path, info: &VersionInfo, bytes: &[u8], base: Option<&pack::Manifest>) -> Result<u64, String> {
    let (parts, comment) = pack::split_zip(bytes).ok_or("not a zip package that can be kept as parts")?;
    let manifest = pack::Manifest {
        format: 1,
        info: VersionInfo { storage: VersionStorage::Parts, stored: None, ..info.clone() },
        comment,
        parts: parts.iter().map(|part| part.meta.clone()).collect(),
    };
    let contents: Vec<Vec<u8>> = parts.iter().map(|part| part.data.clone()).collect();
    let rebuilt = pack::assemble_zip(&manifest, &contents)?;
    drop(contents);
    let same = pack::split_zip(&rebuilt)
        .is_some_and(|(again, again_comment)| again_comment == manifest.comment && again.iter().map(|part| &part.meta).eq(manifest.parts.iter()));
    if !same {
        return Err("the package does not come back the same when rebuilt".into());
    }
    let added = store_parts(dir, &parts, base)?;
    let json = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    write_atomic(&manifest_path(dir, &info.id), &json)?;
    Ok(added.saturating_add(json.len() as u64))
}

/// The bytes of a version kept as parts: the archive rebuilt from its manifest. Every part is
/// checked against its hash; the zip around them may differ from the saved file's bytes.
#[cfg(not(target_arch = "wasm32"))]
fn read_parts_version(dir: &Path, id: &str) -> Result<Vec<u8>, String> {
    let manifest = read_manifest(dir, id).ok_or("this version's list of parts is missing or damaged")?;
    let mut contents = Vec::with_capacity(manifest.parts.len());
    for part in &manifest.parts {
        if part.dir {
            contents.push(Vec::new());
            continue;
        }
        let content = load_object(dir, &part.sha256)?;
        if content.len() as u64 != part.size {
            return Err("a part of this version is damaged".into());
        }
        contents.push(content);
    }
    pack::assemble_zip(&manifest, &contents)
}

/// Versions written whole by earlier builds move to parts a little at a time: at most one per
/// save, newest first, and only after the parts read back. One that cannot be split (not a
/// zip, or over the limits) is marked with `stored` so it is not tried again.
#[cfg(not(target_arch = "wasm32"))]
fn migrate_one(dir: &Path, list: &mut [VersionInfo]) {
    let base = newest_manifest(dir, list);
    let Some(entry) = list.iter_mut().find(|v| v.storage == VersionStorage::Whole && v.stored.is_none() && v.kind != VersionKind::Original) else {
        return;
    };
    let mut converted = None;
    if let Some(bytes) = read_small(&dir.join(format!("{}.bin", entry.id)), MAX_DOCUMENT_BYTES)
        && bytes.starts_with(b"PK\x03\x04")
    {
        let digest = digest_hex(&bytes);
        // A damaged `.bin` stays as it is (reading it reports the damage).
        if entry.digest.as_ref().is_none_or(|known| *known == digest) {
            entry.digest = Some(digest);
            converted = write_parts_version(dir, entry, &bytes, base.as_ref()).ok();
        }
    }
    match converted {
        // The `.bin` goes when the index no longer needs it (see `prune`).
        Some(stored) => {
            entry.storage = VersionStorage::Parts;
            entry.stored = Some(stored);
        }
        None => {
            let _ = fs::remove_file(manifest_path(dir, &entry.id));
            entry.stored = Some(entry.size);
        }
    }
}

/// What versions take on disk, with shared parts counted once.
#[cfg(not(target_arch = "wasm32"))]
struct Usage<'a> {
    dir: &'a Path,
    objects: std::collections::HashMap<String, (u64, Option<String>)>,
    /// Something could not be read: object files are then left alone this time.
    incomplete: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl<'a> Usage<'a> {
    fn new(dir: &'a Path) -> Self {
        Usage { dir, objects: std::collections::HashMap::new(), incomplete: false }
    }

    /// The objects a version needs (its parts and the bases of their deltas) and the size of
    /// its own file.
    fn needs(&mut self, entry: &VersionInfo) -> (BTreeSet<String>, u64) {
        let mut needed = BTreeSet::new();
        if entry.storage == VersionStorage::Whole {
            return (needed, entry.size);
        }
        let own = fs::symlink_metadata(manifest_path(self.dir, &entry.id)).map_or(0, |m| m.len());
        let Some(manifest) = read_manifest(self.dir, &entry.id) else {
            self.incomplete = true;
            return (needed, own);
        };
        for part in manifest.parts.iter().filter(|part| !part.dir) {
            let mut current = Some(part.sha256.clone());
            let mut steps = 0;
            while let Some(hash) = current.take() {
                steps += 1;
                if steps > pack::MAX_CHAIN || !needed.insert(hash.clone()) {
                    break;
                }
                if !self.objects.contains_key(&hash) {
                    match object_header(self.dir, &hash) {
                        Ok((header, len)) => {
                            self.objects.insert(hash.clone(), (len, header.base));
                        }
                        Err(_) => {
                            self.incomplete = true;
                            continue;
                        }
                    }
                }
                current = self.objects.get(&hash).and_then(|(_, base)| base.clone());
            }
        }
        (needed, own)
    }

    fn size(&self, hash: &str) -> u64 {
        self.objects.get(hash).map_or(0, |(len, _)| *len)
    }
}

/// The time retention counts from: the capture time of an `original`, else when it was saved.
#[cfg(not(target_arch = "wasm32"))]
fn retention_secs(entry: &VersionInfo) -> Option<u64> {
    entry.captured_at.as_deref().and_then(parse_iso_secs).or_else(|| parse_iso_secs(&entry.saved_at))
}

/// Apply the retention rules and delete the version files that fall out:
/// - every version kept for [`KEEP_ALL_SECS`], then the newest of each hour until
///   [`KEEP_HOURLY_SECS`], of each day until [`KEEP_DAILY_SECS`] and of each week after that;
/// - at most [`MAX_VERSIONS`] versions and [`MAX_DOCUMENT_BYTES`] of real disk space per
///   document (a part shared by several versions counts once), newest first;
/// - the oldest `original` (the file as it was before WordCraft first wrote it) is pinned: it
///   outlives the time rules and is the last to go for the count and size limits. This goes
///   beyond OneDrive on purpose: AutoSave is on for local files that never had a server-side
///   history, so this copy is the only way back to what was there.
#[cfg(not(target_arch = "wasm32"))]
fn prune(dir: &Path, mut list: Vec<VersionInfo>, now: u64) -> Vec<VersionInfo> {
    sort_newest_first(&mut list);
    let pinned = list.iter().rev().find(|v| v.kind == VersionKind::Original).map(|v| v.id.clone());
    let room = if pinned.is_some() { MAX_VERSIONS.saturating_sub(1) } else { MAX_VERSIONS };
    let mut kept = Vec::new();
    let mut pinned_entry = None;
    let mut periods = BTreeSet::new();
    for entry in list {
        if pinned.as_deref() == Some(entry.id.as_str()) {
            pinned_entry = Some(entry);
            continue;
        }
        if kept.len() >= room {
            continue;
        }
        let age = now.saturating_sub(retention_secs(&entry).unwrap_or(now));
        let at = parse_iso_secs(&entry.saved_at).unwrap_or(0);
        // Hours, days and weeks since the epoch; weeks start on Monday (the epoch was a Thursday).
        let period = if age <= KEEP_ALL_SECS {
            None
        } else if age <= KEEP_HOURLY_SECS {
            Some((0u8, at / 3_600))
        } else if age <= KEEP_DAILY_SECS {
            Some((1, at / DAY_SECS))
        } else {
            Some((2, (at / DAY_SECS).saturating_add(3) / 7))
        };
        if period.is_none_or(|period| periods.insert(period)) {
            kept.push(entry);
        }
    }
    // Real space: each version's own file plus the objects it needs, shared objects once.
    let mut usage = Usage::new(dir);
    let mut needs: Vec<(BTreeSet<String>, u64)> = kept.iter().map(|entry| usage.needs(entry)).collect();
    let pinned_needs = pinned_entry.as_ref().map(|entry| usage.needs(entry));
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut total = 0u64;
    for (objects, own) in needs.iter().chain(pinned_needs.iter()) {
        total = total.saturating_add(*own);
        for hash in objects {
            let count = counts.entry(hash.clone()).or_insert(0);
            if *count == 0 {
                total = total.saturating_add(usage.size(hash));
            }
            *count += 1;
        }
    }
    // The newest version always stays, even alone over the budget.
    while total > MAX_DOCUMENT_BYTES && kept.len() > 1 {
        kept.pop();
        let Some((objects, own)) = needs.pop() else { break };
        total = total.saturating_sub(own);
        for hash in objects {
            if let Some(count) = counts.get_mut(&hash) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    total = total.saturating_sub(usage.size(&hash));
                }
            }
        }
    }
    if let Some(entry) = pinned_entry {
        kept.push(entry);
        sort_newest_first(&mut kept);
    }
    let keep: std::collections::HashMap<&str, VersionStorage> = kept.iter().map(|v| (v.id.as_str(), v.storage)).collect();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            // A version's file goes when the version goes, or when it is kept the other way.
            let stale = |id: &str, storage: VersionStorage| id_ok(id) && keep.get(id) != Some(&storage);
            if name.strip_suffix(".bin").is_some_and(|id| stale(id, VersionStorage::Whole))
                || name.strip_suffix(PARTS_SUFFIX).is_some_and(|id| stale(id, VersionStorage::Parts))
            {
                let _ = fs::remove_file(path);
            }
        }
    }
    // Objects no kept version needs, directly or as the base of a delta. When anything could
    // not be read, nothing is collected: an unreadable manifest must not cost its parts.
    if !usage.incomplete
        && let Ok(entries) = fs::read_dir(dir.join(OBJECTS_DIR))
    {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if pack::hash_ok(name) && counts.get(name).is_none_or(|count| *count == 0) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    kept
}

#[cfg(not(target_arch = "wasm32"))]
fn store_save(root: &Path, write: VersionWrite) -> Result<Vec<VersionInfo>, String> {
    let now = crate::cmd::now_unix();
    store_save_at(root, write, now)
}

#[cfg(not(target_arch = "wasm32"))]
fn store_save_at(root: &Path, write: VersionWrite, now: u64) -> Result<Vec<VersionInfo>, String> {
    let size = u64::try_from(write.bytes.len()).map_err(|_| "version is too large".to_string())?;
    if size > MAX_DOCUMENT_BYTES {
        return Err("version exceeds the 500 MiB document history limit".into());
    }
    let dir = document_dir(root, &write.path);
    private_dir(&dir).map_err(|e| e.to_string())?;
    let mut list = read_index(&dir);
    let digest = digest_hex(&write.bytes);
    let window_start = |v: &VersionInfo| v.window_start.clone().unwrap_or_else(|| v.saved_at.clone());
    // AutoSave joins the newest version only when its own editing session opened that window
    // less than AUTOSAVE_WINDOW_SECS ago.
    let replace = write.kind == VersionKind::Autosave
        && write.session.is_some()
        && list.first().is_some_and(|v| {
            v.kind == VersionKind::Autosave
                && v.session == write.session
                && now.saturating_sub(parse_iso_secs(&window_start(v)).unwrap_or(0)) < AUTOSAVE_WINDOW_SECS
        });
    let started = if replace { list.first().map(window_start) } else { None };
    let id = if replace { list.first().map(|v| v.id.clone()).ok_or("autosave history is inconsistent")? } else { new_id()? };
    let captured_at = (write.kind == VersionKind::Original).then(|| crate::cmd::iso_from_unix_secs(now));
    let session = if write.kind == VersionKind::Autosave { write.session } else { None };
    let mut info = VersionInfo {
        id: id.clone(),
        saved_at: write.saved_at,
        author: write.author,
        size,
        kind: write.kind,
        window_start: started,
        digest: Some(digest),
        session,
        captured_at,
        storage: VersionStorage::Whole,
        stored: Some(size),
    };
    // A zip package is kept as parts; the `original` copy is kept byte for byte, as it was on
    // disk. Anything that goes wrong with the parts falls back to the whole bytes.
    let as_parts = write.kind != VersionKind::Original
        && write.bytes.starts_with(b"PK\x03\x04")
        // An AutoSave that replaces the newest version deltas against the one before it, so
        // the replaced version's parts are not kept alive as a base.
        && match write_parts_version(&dir, &info, &write.bytes, newest_manifest(&dir, if replace { list.get(1..).unwrap_or_default() } else { &list }).as_ref()) {
            Ok(stored) => {
                info.storage = VersionStorage::Parts;
                info.stored = Some(stored);
                true
            }
            Err(error) => {
                log::debug!("version history keeps this version whole: {error}");
                false
            }
        };
    if !as_parts {
        write_version_file(&dir, &id, &write.bytes)?;
    }
    if replace {
        if let Some(first) = list.first_mut() {
            *first = info;
        }
    } else {
        list.insert(0, info);
    }
    migrate_one(&dir, &mut list);
    list = prune(&dir, list, now);
    save_index(&dir, &list)?;
    // The identity record only helps find a moved file; failing to write it loses nothing.
    let _ = write_meta(&dir, &write.path);
    Ok(list)
}

/// The digest of a stored version: from the index, or computed from its file for older entries.
#[cfg(not(target_arch = "wasm32"))]
fn entry_digest(dir: &Path, entry: &VersionInfo) -> Option<String> {
    if let Some(digest) = &entry.digest {
        return Some(digest.clone());
    }
    let bytes = read_small(&dir.join(format!("{}.bin", entry.id)), MAX_DOCUMENT_BYTES)?;
    Some(digest_hex(&bytes))
}

/// Move the history in `from_dir` to `to_dir` (the bucket of `to_path`), merging with whatever
/// `to_dir` already holds. Version files keep their random ids, so merging never overwrites.
#[cfg(not(target_arch = "wasm32"))]
fn merge_bucket(from_dir: &Path, to_dir: &Path, to_path: &Path, now: u64) -> Result<Vec<VersionInfo>, String> {
    if from_dir == to_dir {
        return Ok(read_index(to_dir));
    }
    let moving = read_index(from_dir);
    if moving.is_empty() {
        return Ok(read_index(to_dir));
    }
    if fs::symlink_metadata(to_dir).is_err() && fs::rename(from_dir, to_dir).is_ok() {
        let _ = write_meta(to_dir, to_path);
        return Ok(read_index(to_dir));
    }
    private_dir(to_dir).map_err(|e| e.to_string())?;
    let mut list = read_index(to_dir);
    let mut left_behind = false;
    // Objects first, so a moved manifest never names a part that is not there yet. An object
    // with the same name holds the same content, so one already there is kept.
    if let Ok(objects) = fs::read_dir(from_dir.join(OBJECTS_DIR)) {
        private_dir(&to_dir.join(OBJECTS_DIR)).map_err(|e| e.to_string())?;
        for object in objects.flatten() {
            let name = object.file_name();
            if !name.to_str().is_some_and(pack::hash_ok) {
                continue;
            }
            let target = to_dir.join(OBJECTS_DIR).join(&name);
            if fs::symlink_metadata(&target).is_ok() {
                continue;
            }
            let source = object.path();
            let moved = fs::rename(&source, &target).is_ok() || fs::copy(&source, &target).is_ok();
            if !moved {
                left_behind = true;
            }
        }
    }
    for entry in moving {
        let name = match entry.storage {
            VersionStorage::Whole => format!("{}.bin", entry.id),
            VersionStorage::Parts => format!("{}{PARTS_SUFFIX}", entry.id),
        };
        if list.iter().any(|v| v.id == entry.id) {
            continue;
        }
        let (source, target) = (from_dir.join(&name), to_dir.join(&name));
        let expected = fs::symlink_metadata(&source).map_or(0, |m| m.len());
        let moved = fs::rename(&source, &target).is_ok()
            || (fs::copy(&source, &target).is_ok_and(|copied| copied == expected) && fs::remove_file(&source).is_ok());
        if moved {
            list.push(entry);
        } else {
            left_behind = true;
        }
    }
    let list = prune(to_dir, list, now);
    save_index(to_dir, &list)?;
    let _ = write_meta(to_dir, to_path);
    if !left_behind {
        let _ = fs::remove_dir_all(from_dir);
    }
    Ok(list)
}

/// The history of a file moved or renamed outside WordCraft. A bucket qualifies when the file it
/// last belonged to no longer exists (a copy keeps its original, so a copy never takes the
/// history, as with OneDrive, where only a move keeps it) and its newest version holds exactly
/// `digest`, the bytes now at `path`. Several candidates: the most recently saved wins.
#[cfg(not(target_arch = "wasm32"))]
fn adopt_moved(root: &Path, path: &Path, digest: &str, now: u64) -> Option<Vec<VersionInfo>> {
    let target = document_dir(root, path);
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in fs::read_dir(root).ok()?.flatten().take(MAX_BUCKET_SCAN) {
        let dir = entry.path();
        if dir == target || !entry.file_name().to_str().is_some_and(key_ok) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Some(meta) = read_meta(&dir) else { continue };
        let previous = PathBuf::from(&meta.path);
        if previous == path || fs::symlink_metadata(&previous).is_ok() {
            continue;
        }
        let list = read_index(&dir);
        let Some(newest) = list.first() else { continue };
        if newest.digest.as_deref() != Some(digest) {
            continue;
        }
        let saved = parse_iso_secs(&newest.saved_at).unwrap_or(0);
        if best.as_ref().is_none_or(|(at, _)| saved > *at) {
            best = Some((saved, dir));
        }
    }
    let (_, dir) = best?;
    merge_bucket(&dir, &target, path, now).ok().filter(|list| !list.is_empty())
}

/// Read the document file at `path` for the history (at most [`MAX_DOCUMENT_BYTES`]).
#[cfg(not(target_arch = "wasm32"))]
fn read_document(path: &Path) -> Result<Option<(Vec<u8>, fs::Metadata)>, String> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !meta.is_file() {
        return Ok(None);
    }
    if meta.len() > MAX_DOCUMENT_BYTES {
        return Err("the file is larger than the 500 MiB document history limit; its current bytes were not kept".into());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err("the file is larger than the 500 MiB document history limit".into());
    }
    Ok(Some((bytes, meta)))
}

/// Make sure the bytes at `path` are in its history before a save replaces them, storing them
/// as an `original` version when they are not.
#[cfg(not(target_arch = "wasm32"))]
fn store_protect(root: &Path, path: &Path, now: u64) -> Result<(), String> {
    let Some((bytes, meta)) = read_document(path)? else { return Ok(()) };
    if bytes.is_empty() {
        // An empty file (File Explorer's New › Word document) has nothing to go back to.
        return Ok(());
    }
    let digest = digest_hex(&bytes);
    let dir = document_dir(root, path);
    let mut list = read_index(&dir);
    if list.is_empty()
        && let Some(adopted) = adopt_moved(root, path, &digest, now)
    {
        list = adopted;
    }
    if list.iter().any(|entry| entry_digest(&dir, entry).as_deref() == Some(digest.as_str())) {
        return Ok(());
    }
    let saved_at = modified_secs(&meta).map_or(now, |at| at.min(now));
    store_save_at(
        root,
        VersionWrite {
            path: path.to_path_buf(),
            bytes,
            author: String::new(),
            kind: VersionKind::Original,
            saved_at: crate::cmd::iso_from_unix_secs(saved_at),
            session: None,
        },
        now,
    )
    .map(|_| ())
}

#[cfg(not(target_arch = "wasm32"))]
fn store_list(root: &Path, path: &Path, probe: bool) -> Result<Vec<VersionInfo>, String> {
    let dir = document_dir(root, path);
    let list = read_index(&dir);
    if !list.is_empty() || !probe {
        return Ok(list);
    }
    let Ok(Some((bytes, _))) = read_document(path) else { return Ok(list) };
    if bytes.is_empty() {
        return Ok(list);
    }
    Ok(adopt_moved(root, path, &digest_hex(&bytes), crate::cmd::now_unix()).unwrap_or(list))
}

#[cfg(not(target_arch = "wasm32"))]
fn store_rename(root: &Path, from: &Path, to: &Path) -> Result<Vec<VersionInfo>, String> {
    merge_bucket(&document_dir(root, from), &document_dir(root, to), to, crate::cmd::now_unix())
}

#[cfg(not(target_arch = "wasm32"))]
fn hold(lock: &Mutex<()>) -> std::sync::MutexGuard<'_, ()> {
    lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(not(target_arch = "wasm32"))]
impl Versions {
    pub fn start(root: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        private_dir(&root).map_err(|e| e.to_string())?;
        let (jobs, rx_jobs) = mpsc::sync_channel(8);
        let (events, rx) = mpsc::channel();
        let worker_root = root.clone();
        let lock = Arc::new(Mutex::new(()));
        let worker_lock = Arc::clone(&lock);
        std::thread::Builder::new()
            .name("wordcraft-versions".into())
            .spawn(move || {
                while let Ok(job) = rx_jobs.recv() {
                    let guard = hold(&worker_lock);
                    let (path, result) = match job {
                        Job::Save(write) => {
                            let path = write.path.clone();
                            let result = store_save(&worker_root, write);
                            (path, result)
                        }
                        Job::List { path, probe } => {
                            let result = store_list(&worker_root, &path, probe);
                            (path, result)
                        }
                        Job::Rename { from, to } => {
                            let result = store_rename(&worker_root, &from, &to);
                            (to, result)
                        }
                    };
                    drop(guard);
                    let (list, result) = match result {
                        Ok(list) => (list, Ok(())),
                        Err(error) => (Vec::new(), Err(error)),
                    };
                    if events.send(Event { path, list, result }).is_err() {
                        break;
                    }
                    wake();
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            tx: jobs,
            rx,
            root,
            lock,
            run_id: new_id()?,
            verified: None,
            list: Vec::new(),
            ready: false,
            busy: false,
            error: None,
            current_path: None,
            alive: true,
        })
    }

    fn queue(&mut self, job: Job) -> Result<(), String> {
        if !self.alive {
            return Err("version history worker is unavailable".into());
        }
        self.tx.try_send(job).map_err(|e| e.to_string())?;
        self.busy = true;
        self.error = None;
        Ok(())
    }

    pub fn poll(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    self.busy = false;
                    self.ready = true;
                    self.current_path = Some(event.path);
                    match event.result {
                        Ok(()) => {
                            self.list = event.list;
                        }
                        Err(error) => {
                            self.error = Some(error);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.busy = false;
                    self.alive = false;
                    if self.error.is_none() {
                        self.error = Some("version history worker is unavailable".into());
                    }
                    break;
                }
            }
        }
    }

    /// Show the history of `path`. The index is small, so it is read here at once and the
    /// first `file.versions` after opening a document already lists its versions. Only when
    /// there are none does the worker look for the history of a moved file in the background.
    pub fn ensure_path(&mut self, path: &Path) {
        let canonical = canonical_path(path);
        if self.current_path.as_deref() == Some(canonical.as_path()) {
            return;
        }
        // Version files are replaced by rename and opened with sharing, so reading the index
        // while the worker writes never blocks either side.
        self.list = read_index(&document_dir(&self.root, &canonical));
        self.ready = true;
        self.current_path = Some(canonical.clone());
        if !self.list.is_empty() {
            return;
        }
        // The worker may read the document to find a moved file's history; only where this
        // thread may read it (automation roots).
        let probe = crate::file_access::check_read(&canonical.to_string_lossy()).is_ok();
        if probe && let Err(error) = self.queue(Job::List { path: canonical, probe }) {
            self.error = Some(error);
        }
    }

    pub fn save(&mut self, path: &Path, bytes: Vec<u8>, author: &str, kind: VersionKind, saved_at: String, session: Option<String>) {
        let path = canonical_path(path);
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            self.error = Some("version exceeds the 500 MiB document history limit".into());
            return;
        }
        if let Err(error) = self.queue(Job::Save(VersionWrite { path, bytes, author: author.to_string(), kind, saved_at, session })) {
            self.error = Some(error);
        }
    }

    /// The history follows a document `file.rename` moved, in order with the saves queued
    /// before it.
    pub fn renamed(&mut self, from: PathBuf, to: PathBuf) {
        let to = canonical_path(&to);
        if let Err(error) = self.queue(Job::Rename { from, to }) {
            // The bucket stays under the old name; the moved-file search still finds it at the
            // next save, since the old path no longer exists.
            self.error = Some(error);
        }
    }

    /// Synchronously keep the bytes at `path` before they are overwritten (see [`store_protect`]).
    fn protect(&mut self, document_id: u64, path: &Path) -> Result<(), String> {
        let canonical = canonical_path(path);
        if self.verified.as_ref().is_some_and(|(id, verified)| *id == document_id && *verified == canonical) {
            return Ok(());
        }
        let result = {
            let _guard = hold(&self.lock);
            store_protect(&self.root, &canonical, crate::cmd::now_unix())
        };
        if result.is_ok() {
            self.verified = Some((document_id, canonical));
        }
        result
    }

    pub fn read(&self, path: &Path, id: &str) -> Result<(VersionInfo, Vec<u8>), String> {
        if !id_ok(id) {
            return Err("invalid version id".into());
        }
        let dir = document_dir(&self.root, path);
        // Held while parts are read, so the worker's collection of unused parts cannot remove
        // one in between.
        let guard = hold(&self.lock);
        let list = read_index(&dir);
        let info = list.into_iter().find(|entry| entry.id == id).ok_or_else(|| "no such version".to_string())?;
        if info.storage == VersionStorage::Parts {
            let bytes = read_parts_version(&dir, id)?;
            return Ok((info, bytes));
        }
        drop(guard);
        let file_path = dir.join(format!("{id}.bin"));
        let meta = fs::symlink_metadata(&file_path).map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.len() != info.size || meta.len() > MAX_DOCUMENT_BYTES {
            return Err("version bytes are missing or invalid".into());
        }
        let file = File::open(&file_path).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.take(MAX_DOCUMENT_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() as u64 != info.size {
            return Err("version bytes are truncated".into());
        }
        if info.digest.as_deref().is_some_and(|digest| digest != digest_hex(&bytes)) {
            return Err("version bytes are damaged (their checksum does not match)".into());
        }
        Ok((info, bytes))
    }
}

impl crate::Session {
    /// Desktop host wiring for the local history folder.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn enable_versions(&mut self, root: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Result<(), String> {
        self.version_store = Some(Versions::start(root, wake)?);
        Ok(())
    }

    pub fn versions_tick(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = self.version_store.as_mut() {
            store.poll();
        }
    }

    /// The last error of the history store, if any (a failed history write never fails a save).
    pub fn versions_error(&mut self) -> Option<String> {
        self.versions_tick();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = self.version_store.as_ref() {
            return store.error.clone();
        }
        None
    }

    pub fn version_infos(&mut self) -> Vec<VersionInfo> {
        self.versions_tick();
        #[cfg(not(target_arch = "wasm32"))]
        if let (Some(path), Some(store)) =
            (self.opened_version.as_ref().map(|version| version.path.clone()).or_else(|| self.path.clone()), self.version_store.as_mut())
        {
            store.ensure_path(&path);
            return store.list.clone();
        }
        Vec::new()
    }

    /// The editing session AutoSave versions are grouped by: this run of the app and this
    /// opening of the document.
    #[cfg(not(target_arch = "wasm32"))]
    fn version_session(&self) -> Option<String> {
        self.version_store.as_ref().map(|store| format!("{}-{}", store.run_id, self.document_id()))
    }

    pub(crate) fn record_version(&mut self, path: &Path, bytes: Vec<u8>, kind: VersionKind) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let session = if kind == VersionKind::Autosave { self.version_session() } else { None };
            if let Some(store) = self.version_store.as_mut() {
                store.save(path, bytes, &self.author, kind, crate::cmd::now_iso(), session);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (path, bytes, kind);
    }

    /// Before a save writes over `path`: keep the bytes there in the history if they are not in
    /// it yet (a file with no history, or one another program changed since WordCraft last read
    /// or wrote it). Runs synchronously, so the copy is on disk before the file is replaced. It
    /// reads the file once per opened document and path; after that it only checks the file's
    /// stamp. Any failure is recorded in the store's error and the save goes ahead.
    pub(crate) fn protect_before_overwrite(&mut self, path: &Path) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let document_id = self.document_id();
            let own_file = self.path.as_deref().is_some_and(|own| own == path || canonical_path(own) == canonical_path(path));
            let changed_outside =
                own_file && self.cloud.stamp.as_mut().is_some_and(|stamp| !matches!(stamp.check(path), Ok(wordcraft_cloud::DiskState::Unchanged)));
            let Some(store) = self.version_store.as_mut() else { return };
            if changed_outside {
                store.verified = None;
            }
            if crate::file_access::check_read(&path.to_string_lossy()).is_err() {
                return;
            }
            if let Err(error) = store.protect(document_id, path) {
                log::warn!("version history could not keep {} before saving: {error}", path.display());
                store.error = Some(error);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = path;
    }

    /// After a save: the file at `path` now holds bytes the history is about to receive.
    #[cfg(not(target_arch = "wasm32"))]
    fn mark_verified(&mut self, path: &Path) {
        let document_id = self.document_id();
        if let Some(store) = self.version_store.as_mut() {
            store.verified = Some((document_id, canonical_path(path)));
        }
    }

    /// `file.rename` moved the document from `from` (its canonical path, taken before the move)
    /// to `to`: the history goes with it, as OneDrive keeps a file's history through a rename
    /// or move.
    pub(crate) fn versions_renamed(&mut self, from: PathBuf, to: &Path) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let document_id = self.document_id();
            if let Some(store) = self.version_store.as_mut() {
                let verified = store.verified.as_ref().is_some_and(|(id, path)| *id == document_id && *path == from);
                store.renamed(from, to.to_path_buf());
                store.verified = verified.then(|| (document_id, canonical_path(to)));
                store.current_path = None;
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (from, to);
    }

    pub(crate) fn record_saved_command(&mut self, id: &str, params: &serde_json::Value, result: &serde_json::Value) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.version_store.is_none()
                || !matches!(id, "file.save" | "file.saveAs" | "file.saveToCloud" | "file.resolveConflict")
                || result.get("saved").and_then(serde_json::Value::as_bool) != Some(true)
            {
                return;
            }
            let result_path = result.get("path").and_then(serde_json::Value::as_str).map(PathBuf::from);
            let Some(bytes) = result_path
                .as_deref()
                .and_then(crate::io::take_last_saved_bytes)
                .or_else(|| self.path.as_deref().and_then(crate::io::take_last_saved_bytes))
            else {
                return;
            };
            let path = result_path.or_else(|| self.path.clone()).or_else(|| self.opened_version.as_ref().map(|v| v.path.clone()));
            let Some(path) = path else { return };
            // A copy in another format (Save As PDF, plain text…) is an export, not a version of
            // this document: only the document's own file has a history.
            if !self.path.as_deref().is_some_and(|own| own == path || canonical_path(own) == canonical_path(&path)) {
                return;
            }
            let autosave = id == "file.save" && params.get("ifUnchanged").and_then(serde_json::Value::as_bool) == Some(true);
            let kind = self.take_version_kind(autosave);
            self.mark_verified(&path);
            self.record_version(&path, bytes, kind);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (id, params, result);
    }

    /// Restore is about to replace `current`, which holds changes not yet written to its file:
    /// store them as a version first, so the state before the restore stays in the
    /// history (OneDrive likewise keeps the replaced current version in the list).
    pub(crate) fn keep_before_restore(&mut self, source: &Path, current: &wordcraft_doc::Document) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.version_store.is_none() {
                return;
            }
            let password = self.password.as_ref().map(crate::io::Password::as_str);
            match crate::io::save_bytes_with(&source.to_string_lossy(), current, password, &crate::io::ExportOptions::default()) {
                // `Autosave` without a session: never joined into, nor joining, another version.
                Ok(bytes) => {
                    if let Some(store) = self.version_store.as_mut() {
                        store.save(source, bytes, &self.author, VersionKind::Autosave, crate::cmd::now_iso(), None);
                    }
                }
                Err(error) => {
                    if let Some(store) = self.version_store.as_mut() {
                        store.error = Some(error);
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (source, current);
    }

    /// Restore replaced the document with the version `info`: record the restore at once with
    /// those exact bytes, as OneDrive makes the restored version the newest one in the list
    /// right away (whether or not AutoSave writes the file afterwards).
    pub(crate) fn record_restore(&mut self, source: &Path, bytes: Vec<u8>) {
        self.record_version(source, bytes, VersionKind::Restore);
    }

    pub(crate) fn read_version(&mut self, path: &Path, id: &str) -> Result<(VersionInfo, Vec<u8>), String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.versions_tick();
            let store = self.version_store.as_ref().ok_or_else(|| "local version history is unavailable".to_string())?;
            store.read(path, id)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (path, id);
            Err("local version history is available on desktop only".into())
        }
    }

    pub fn previous_version(&self) -> Option<&VersionInfo> {
        self.opened_version.as_ref().map(|version| &version.info)
    }

    pub(crate) fn take_version_kind(&mut self, autosave: bool) -> VersionKind {
        if autosave {
            self.version_next_kind.take().unwrap_or(VersionKind::Autosave)
        } else {
            self.version_next_kind = None;
            VersionKind::Save
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_key_is_stable_and_bounded() {
        let path = Path::new("relative/notes.docx");
        assert_eq!(document_key(path), document_key(path));
        assert_eq!(document_key(path).len(), 16);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn iso_parser_accepts_engine_timestamps() {
        let now = 1_800_000_000;
        let value = crate::cmd::iso_from_unix_secs(now);
        assert_eq!(parse_iso_secs(&value), Some(now));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn temp(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("wordcraft-versions-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn write_in(root: &Path, file: &Path, text: &str, kind: VersionKind, when: u64, now: u64, session: Option<&str>) -> Vec<VersionInfo> {
        store_save_at(
            root,
            VersionWrite {
                path: file.to_path_buf(),
                bytes: text.as_bytes().to_vec(),
                author: "Tester".into(),
                kind,
                saved_at: crate::cmd::iso_from_unix_secs(when),
                session: session.map(str::to_owned),
            },
            now,
        )
        .unwrap()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn write(root: &Path, file: &Path, text: &str, kind: VersionKind, when: u64, now: u64) -> Vec<VersionInfo> {
        write_in(root, file, text, kind, when, now, Some("run-1"))
    }

    /// A session with the history enabled in `root/versions`, never the real profile.
    #[cfg(not(target_arch = "wasm32"))]
    fn session_in(root: &Path, text: &str) -> crate::Session {
        let mut session = crate::Session::new(wordcraft_doc::Document::from_text(text));
        session.version_store = Some(Versions::start(root.join("versions"), std::sync::Arc::new(|| {})).unwrap());
        session
    }

    /// The stored versions of `file` once the worker has written at least `count` of them.
    #[cfg(not(target_arch = "wasm32"))]
    fn wait_versions(session: &crate::Session, file: &Path, count: usize) -> Vec<VersionInfo> {
        let store = session.version_store.as_ref().unwrap();
        let dir = document_dir(&store.root, file);
        for _ in 0..500 {
            let list = {
                let _guard = hold(&store.lock);
                read_index(&dir)
            };
            if list.len() >= count {
                return list;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _guard = hold(&store.lock);
        read_index(&dir)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn version_bytes(session: &crate::Session, file: &Path, id: &str) -> Vec<u8> {
        session.version_store.as_ref().unwrap().read(file, id).unwrap().1
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn body(session: &crate::Session) -> String {
        session.doc.plain_text(wordcraft_doc::StoryRef::Body)
    }

    /// Two zip packages with the same parts: names, order, methods, dates and contents.
    #[cfg(not(target_arch = "wasm32"))]
    fn same_parts(a: &[u8], b: &[u8]) -> bool {
        let parts = |bytes: &[u8]| pack::split_zip(bytes).map(|(parts, comment)| (parts.into_iter().map(|p| p.meta).collect::<Vec<_>>(), comment));
        parts(a).is_some() && parts(a) == parts(b)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    /// The body of a document with `paragraphs` paragraphs, the `edit`-th one changed.
    #[cfg(not(target_arch = "wasm32"))]
    fn body_xml(paragraphs: usize, edit: usize) -> Vec<u8> {
        let mut xml = String::from("<?xml version=\"1.0\"?><w:document><w:body>");
        for i in 0..paragraphs {
            let words = if i == edit % paragraphs.max(1) {
                format!("Paragraph {i} as revised in edit {edit}, with a new sentence.")
            } else {
                format!("Paragraph {i} of the report says what it says.")
            };
            xml.push_str(&format!(
                "<w:p><w:pPr><w:pStyle w:val=\"Normal\"/></w:pPr><w:r><w:rPr><w:lang w:val=\"en-US\"/></w:rPr><w:t>{words}</w:t></w:r></w:p>"
            ));
        }
        xml.push_str("</w:body></w:document>");
        xml.into_bytes()
    }

    /// A body of varied prose (it compresses like real text, not like a repeated line):
    /// `paragraphs` paragraphs of words drawn per paragraph, with the `edit`-th one rewritten.
    #[cfg(not(target_arch = "wasm32"))]
    fn varied_body_xml(paragraphs: usize, edit: usize) -> Vec<u8> {
        const WORDS: &[&str] = &[
            "the",
            "court",
            "found",
            "that",
            "contract",
            "was",
            "signed",
            "after",
            "notice",
            "parties",
            "agreed",
            "payment",
            "within",
            "thirty",
            "days",
            "account",
            "access",
            "restored",
            "evidence",
            "shows",
            "request",
            "denied",
            "claim",
            "damages",
            "moral",
            "platform",
            "user",
            "terms",
            "breach",
            "hearing",
            "judge",
            "appeal",
            "record",
            "filed",
            "deadline",
            "expert",
            "report",
            "witness",
            "statement",
            "judgment",
        ];
        let mut xml = String::from("<?xml version=\"1.0\"?><w:document><w:body>");
        for i in 0..paragraphs {
            let seed = if i == edit % paragraphs.max(1) { (i as u64) << 20 | edit as u64 } else { i as u64 };
            let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let mut words = Vec::new();
            for _ in 0..(12 + (seed % 30)) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                words.push(WORDS[(state % WORDS.len() as u64) as usize]);
            }
            xml.push_str(&format!("<w:p><w:pPr><w:pStyle w:val=\"Normal\"/></w:pPr><w:r><w:t>{}.</w:t></w:r></w:p>", words.join(" ")));
        }
        xml.push_str("</w:body></w:document>");
        xml.into_bytes()
    }

    /// A `.docx`-shaped package: a deflated body and styles and a stored picture, like Word's.
    #[cfg(not(target_arch = "wasm32"))]
    fn package(body: &[u8], picture: &[u8], extra: Option<(&str, &[u8])>) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let mut part = |name: &str, data: &[u8], options| {
            writer.start_file(name, options).unwrap();
            writer.write_all(data).unwrap();
        };
        part("[Content_Types].xml", b"<Types><Default Extension=\"xml\"/><Default Extension=\"png\"/></Types>", deflated);
        part("word/document.xml", body, deflated);
        part("word/styles.xml", "<w:styles>".repeat(2_000).as_bytes(), deflated);
        part("word/media/image1.png", picture, stored);
        if let Some((name, data)) = extra {
            part(name, data, deflated);
        }
        writer.finish().unwrap().into_inner()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_bytes_at(root: &Path, file: &Path, bytes: &[u8], when: u64) -> Vec<VersionInfo> {
        store_save_at(
            root,
            VersionWrite {
                path: file.to_path_buf(),
                bytes: bytes.to_vec(),
                author: "Tester".into(),
                kind: VersionKind::Save,
                saved_at: crate::cmd::iso_from_unix_secs(when),
                session: None,
            },
            when,
        )
        .unwrap()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn read_at(root: &Path, file: &Path, id: &str) -> Result<Vec<u8>, String> {
        let dir = document_dir(root, file);
        match read_index(&dir).into_iter().find(|v| v.id == id).map(|v| v.storage) {
            Some(VersionStorage::Parts) => read_parts_version(&dir, id),
            Some(VersionStorage::Whole) => fs::read(dir.join(format!("{id}.bin"))).map_err(|e| e.to_string()),
            None => Err("no such version".into()),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn objects_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.join(OBJECTS_DIR))
            .map(|entries| entries.flatten().filter_map(|e| e.file_name().to_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn disk_bytes(dir: &Path) -> u64 {
        let mut total = 0;
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let meta = entry.metadata().unwrap();
            total += if meta.is_dir() { disk_bytes(&entry.path()) } else { meta.len() };
        }
        total
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_picture_is_stored_once_and_edits_are_small_deltas() {
        let root = temp("dedupe");
        let file = root.join("Report.docx");
        let picture = noise(300_000, 7);
        let now = 1_800_000_000;
        let first = package(&body_xml(400, 0), &picture, None);
        let list = save_bytes_at(&root, &file, &first, now);
        assert_eq!(list[0].storage, VersionStorage::Parts);
        let first_stored = list[0].stored.unwrap();
        let second = package(&body_xml(400, 1), &picture, None);
        let list = save_bytes_at(&root, &file, &second, now + 60);
        let second_stored = list[0].stored.unwrap();
        assert!(first_stored > picture.len() as u64, "the first version holds the picture");
        assert!(second_stored < 4_000, "an edit costs a small delta, not the picture again: {second_stored} bytes");
        let dir = document_dir(&root, &file);
        let picture_hash = digest_hex(&picture);
        assert!(objects_in(&dir).contains(&picture_hash));
        assert_eq!(object_header(&dir, &picture_hash).unwrap().0.depth, 0);
        // Each version comes back with exactly its own parts.
        assert!(same_parts(&read_at(&root, &file, &list[1].id).unwrap(), &first));
        assert!(same_parts(&read_at(&root, &file, &list[0].id).unwrap(), &second));
        // `size` stays the saved document's size; `size` and `stored` reach file.versions.
        assert_eq!(list[0].size, second.len() as u64);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn changed_parts_chain_deltas_and_start_again_whole() {
        let root = temp("chain");
        let file = root.join("Chain.docx");
        let picture = noise(1_000, 9);
        let now = 1_800_000_000;
        let count = usize::from(pack::KEYFRAME_INTERVAL) + 3;
        let packages: Vec<Vec<u8>> = (0..count).map(|edit| package(&body_xml(200, edit), &picture, None)).collect();
        for (i, bytes) in packages.iter().enumerate() {
            let _ = save_bytes_at(&root, &file, bytes, now + i as u64);
        }
        let dir = document_dir(&root, &file);
        let depths: Vec<u16> = packages
            .iter()
            .map(|bytes| {
                let (parts, _) = pack::split_zip(bytes).unwrap();
                let body = parts.iter().find(|p| p.meta.name == "word/document.xml").unwrap();
                object_header(&dir, &body.meta.sha256).unwrap().0.depth
            })
            .collect();
        let interval = pack::KEYFRAME_INTERVAL;
        let mut expected: Vec<u16> = (0..=interval).collect();
        expected.extend(0..u16::try_from(count).unwrap() - interval - 1);
        assert_eq!(depths, expected, "a whole copy, {interval} deltas, then a whole copy again");
        let list = read_index(&dir);
        for (entry, bytes) in list.iter().rev().zip(&packages) {
            assert!(same_parts(&read_at(&root, &file, &entry.id).unwrap(), bytes));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_damaged_part_only_affects_the_versions_that_need_it() {
        let root = temp("damaged-part");
        let file = root.join("Fragile.docx");
        let picture = noise(2_000, 11);
        let now = 1_800_000_000;
        let packages: Vec<Vec<u8>> = (0..4).map(|edit| package(&body_xml(150, edit), &picture, None)).collect();
        for (i, bytes) in packages.iter().enumerate() {
            let _ = save_bytes_at(&root, &file, bytes, now + i as u64);
        }
        let dir = document_dir(&root, &file);
        let list = read_index(&dir); // newest first
        let body_of = |bytes: &[u8]| pack::split_zip(bytes).unwrap().0.into_iter().find(|p| p.meta.name == "word/document.xml").unwrap().meta.sha256;
        // Damage the third version's body, a delta that the fourth builds on.
        let third = object_path(&dir, &body_of(&packages[2]));
        let mut bytes = fs::read(&third).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x55;
        fs::write(&third, bytes).unwrap();
        assert!(same_parts(&read_at(&root, &file, &list[3].id).unwrap(), &packages[0]), "older versions still open");
        assert!(same_parts(&read_at(&root, &file, &list[2].id).unwrap(), &packages[1]));
        assert!(read_at(&root, &file, &list[1].id).is_err(), "the damaged version is an error");
        assert!(read_at(&root, &file, &list[0].id).is_err(), "and so is the one whose delta leads through it");
        // A missing part is an error too, never a panic.
        fs::remove_file(object_path(&dir, &digest_hex(&picture))).unwrap();
        assert!(read_at(&root, &file, &list[3].id).is_err());
        // Saving the damaged content again writes it whole and repairs every version using it.
        let _ = save_bytes_at(&root, &file, &packages[2], now + 10);
        assert!(same_parts(&read_at(&root, &file, &list[1].id).unwrap(), &packages[2]));
        assert!(same_parts(&read_at(&root, &file, &list[0].id).unwrap(), &packages[3]));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn autosaves_that_join_one_version_leave_no_parts_behind() {
        let root = temp("join-parts");
        let file = root.join("Join.docx");
        let picture = noise(3_000, 23);
        let now = 1_800_000_000;
        let _ = save_bytes_at(&root, &file, &package(&body_xml(150, 0), &picture, None), now);
        let mut last = Vec::new();
        for edit in 1..6 {
            last = package(&body_xml(150, edit), &picture, None);
            let at = now + edit as u64 * 10;
            let _ = store_save_at(
                &root,
                VersionWrite {
                    path: file.clone(),
                    bytes: last.clone(),
                    author: "Tester".into(),
                    kind: VersionKind::Autosave,
                    saved_at: crate::cmd::iso_from_unix_secs(at),
                    session: Some("s".into()),
                },
                at,
            )
            .unwrap();
        }
        let dir = document_dir(&root, &file);
        let list = read_index(&dir);
        assert_eq!(list.len(), 2, "five AutoSave writes joined one version");
        // Content types, styles, the picture, the saved body and the latest body: nothing from
        // the writes the window replaced.
        assert_eq!(objects_in(&dir).len(), 5, "{:?}", objects_in(&dir));
        assert!(same_parts(&read_at(&root, &file, &list[0].id).unwrap(), &last));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn unused_parts_are_collected_but_delta_bases_stay() {
        let root = temp("gc");
        let file = root.join("Collect.docx");
        let picture = noise(5_000, 13);
        let only_first = noise(4_000, 14);
        let now = 1_800_000_000;
        let first = package(&body_xml(150, 0), &picture, Some(("word/media/old.bin", &only_first)));
        let list = save_bytes_at(&root, &file, &first, now);
        let first_id = list[0].id.clone();
        let second = package(&body_xml(150, 1), &picture, None);
        let list = save_bytes_at(&root, &file, &second, now + 1);
        let dir = document_dir(&root, &file);
        let body_first = pack::split_zip(&first).unwrap().0.into_iter().find(|p| p.meta.name == "word/document.xml").unwrap().meta.sha256;
        // The first version goes.
        let kept = prune(&dir, list.into_iter().filter(|v| v.id != first_id).collect(), now + 2);
        assert_eq!(kept.len(), 1);
        assert!(!manifest_path(&dir, &first_id).exists());
        let objects = objects_in(&dir);
        assert!(!objects.contains(&digest_hex(&only_first)), "a part only the removed version had is collected");
        assert!(objects.contains(&body_first), "the base of a delta still in use stays");
        assert!(objects.contains(&digest_hex(&picture)));
        assert!(same_parts(&read_at(&root, &file, &kept[0].id).unwrap(), &second));
        // An unreadable manifest stops collection: its parts are never deleted on a guess.
        fs::write(manifest_path(&dir, &kept[0].id), b"{ damaged").unwrap();
        let before = objects_in(&dir);
        let _ = prune(&dir, kept, now + 3);
        assert_eq!(objects_in(&dir), before);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn whole_versions_from_earlier_builds_stay_readable_and_move_to_parts() {
        let root = temp("migrate");
        let file = root.join("Old.docx");
        let picture = noise(20_000, 15);
        let now = 1_800_000_000;
        let old = package(&body_xml(100, 0), &picture, None);
        // As an earlier build left it: a `.bin` and an index entry without `storage` or `stored`.
        let dir = document_dir(&root, &file);
        fs::create_dir_all(&dir).unwrap();
        let id = "0123456789abcdef0123456789abcdef";
        fs::write(dir.join(format!("{id}.bin")), &old).unwrap();
        let legacy = json!([{"id": id, "savedAt": crate::cmd::iso_from_unix_secs(now), "author": "Tester", "size": old.len(), "kind": "save", "digest": digest_hex(&old)}]);
        fs::write(dir.join(INDEX_FILE), serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(read_at(&root, &file, id).unwrap(), old, "readable as it is");
        let newer = package(&body_xml(100, 1), &picture, None);
        let list = save_bytes_at(&root, &file, &newer, now + 60);
        let migrated = list.iter().find(|v| v.id == id).unwrap();
        assert_eq!(migrated.storage, VersionStorage::Parts, "moved to parts at the next save");
        assert!(!dir.join(format!("{id}.bin")).exists());
        assert!(same_parts(&read_at(&root, &file, id).unwrap(), &old));
        assert_eq!(migrated.digest.as_deref(), Some(digest_hex(&old).as_str()), "still recognised as the bytes that were saved");
        // A `.bin` that is not a zip stays whole and is not tried again.
        let text_file = root.join("notes.txt");
        let list = save_bytes_at(&root, &text_file, b"plain text", now);
        assert_eq!(list[0].storage, VersionStorage::Whole);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_original_and_files_that_are_not_zips_stay_byte_for_byte() {
        let root = temp("verbatim");
        let file = root.join("Original.docx");
        let original = package(&body_xml(50, 0), &noise(1_000, 17), None);
        let list = store_save_at(
            &root,
            VersionWrite {
                path: file.clone(),
                bytes: original.clone(),
                author: String::new(),
                kind: VersionKind::Original,
                saved_at: crate::cmd::iso_from_unix_secs(1_800_000_000),
                session: None,
            },
            1_800_000_000,
        )
        .unwrap();
        assert_eq!(list[0].storage, VersionStorage::Whole);
        assert_eq!(read_at(&root, &file, &list[0].id).unwrap(), original);
        // A truncated or bogus zip is kept whole as well.
        let broken = original.get(..original.len() / 2).unwrap().to_vec();
        let list = save_bytes_at(&root, &file, &broken, 1_800_000_100);
        assert_eq!(list[0].storage, VersionStorage::Whole);
        assert_eq!(read_at(&root, &file, &list[0].id).unwrap(), broken);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_size_limit_counts_real_disk_space() {
        let root = temp("space");
        let file = root.join("Space.docx");
        let dir = document_dir(&root, &file);
        let picture = noise(50_000, 19);
        let now = 1_800_000_000;
        let mut logical = 0u64;
        for edit in 0..20 {
            let bytes = package(&body_xml(100, edit), &picture, None);
            logical += bytes.len() as u64;
            let _ = save_bytes_at(&root, &file, &bytes, now + edit as u64);
        }
        let kept = read_index(&dir);
        assert_eq!(kept.len(), 20);
        let real = disk_bytes(&dir);
        assert!(real * 5 < logical, "20 versions take {real} bytes on disk instead of {logical}");
        let _ = fs::remove_dir_all(root);
    }

    /// The figure for the docs: 100 versions of a ~2 MB `.docx` with a picture, one paragraph
    /// edited each time. Run with `cargo test -p wordcraft-engine --lib storage_figure -- --ignored --nocapture`.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "slow; prints the storage figure for docs/cloud.md"]
    fn storage_figure_for_a_hundred_versions() {
        let root = temp("figure");
        let file = root.join("Figure.docx");
        let dir = document_dir(&root, &file);
        let picture = noise(1_700_000, 21);
        let now = 1_800_000_000;
        let mut whole = 0u64;
        let mut size = 0;
        for edit in 0..100 {
            let bytes = package(&varied_body_xml(3_000, edit * 7), &picture, None);
            size = bytes.len();
            whole += bytes.len() as u64;
            let _ = save_bytes_at(&root, &file, &bytes, now + edit as u64 * 60);
        }
        let parts = disk_bytes(&dir);
        println!(
            "100 versions of a {size}-byte .docx: {whole} bytes kept whole, {parts} bytes as parts ({:.1}%)",
            parts as f64 * 100.0 / whole as f64
        );
        assert!(parts * 10 < whole);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_first_listing_after_opening_is_complete() {
        let root = temp("first-list");
        let file = root.join("Minutes.docx");
        let mut s = session_in(&root, "one");
        s.run("file.save", &json!({"path": file.to_string_lossy()})).unwrap();
        s.run("document.setText", &json!({"text": "two"})).unwrap();
        s.run("file.save", &json!({})).unwrap();
        assert_eq!(wait_versions(&s, &file, 2).len(), 2);
        // A fresh window opens the document and asks at once.
        let mut other = session_in(&root, "");
        other.run("file.open", &json!({"path": file.to_string_lossy()})).unwrap();
        let listed = other.run("file.versions", &json!({})).unwrap();
        assert_eq!(listed.as_array().map(Vec::len), Some(2), "{listed}");
        drop((s, other));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_keys_ignore_case_and_older_folders_move_over() {
        let root = temp("case");
        let file = root.join("Case.docx");
        fs::write(&file, b"x").unwrap();
        let upper = root.join("CASE.DOCX");
        assert_eq!(document_key(&file), document_key(&upper));
        assert_eq!(document_key(&file), document_key(&root.join("case.docx")));
        // History an earlier build kept under the case-sensitive key is found and moved.
        let versions = root.join("versions");
        let legacy = versions.join(legacy_key(&file));
        assert_ne!(legacy, versions.join(document_key(&file)));
        let list = write(&versions, &file, "kept", VersionKind::Save, 1_800_000_000, 1_800_000_000);
        fs::rename(document_dir(&versions, &file), &legacy).unwrap();
        assert_eq!(read_index(&document_dir(&versions, &upper)), list);
        assert!(!legacy.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn autosaves_join_within_ten_minutes_but_not_after() {
        let root = temp("join");
        let file = root.join("note.docx");
        let first = write(&root, &file, "one", VersionKind::Autosave, 1_800_000_000, 1_800_000_000);
        let second = write(&root, &file, "two", VersionKind::Autosave, 1_800_000_599, 1_800_000_599);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].size, 3);
        // The window is counted from its first write, not the latest: continuous editing still
        // leaves a version every ten minutes.
        let third = write(&root, &file, "three", VersionKind::Autosave, 1_800_000_601, 1_800_000_601);
        assert_eq!(third.len(), 2, "ten minutes after the window opened, a new version starts");
        let fourth = write(&root, &file, "four", VersionKind::Autosave, 1_800_001_200, 1_800_001_200);
        assert_eq!(fourth.len(), 2, "joins the window that opened at the third write");
        assert_eq!(fourth[0].size, 4);
        let fifth = write(&root, &file, "five", VersionKind::Autosave, 1_800_000_601 + AUTOSAVE_WINDOW_SECS, 1_800_000_601 + AUTOSAVE_WINDOW_SECS);
        assert_eq!(fifth.len(), 3);
        // An explicit save is always its own version.
        let saved = write(&root, &file, "saved", VersionKind::Save, 1_800_001_202, 1_800_001_202);
        assert_eq!(saved.len(), 4);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn first_autosave_after_reopening_starts_a_new_version() {
        let root = temp("reopen");
        let file = root.join("note.docx");
        let now = 1_800_000_000;
        assert_eq!(write_in(&root, &file, "first run", VersionKind::Autosave, now, now, Some("run-1")).len(), 1);
        // Reopened two minutes later (another session): Word adds a version at the first change.
        let list = write_in(&root, &file, "second run", VersionKind::Autosave, now + 120, now + 120, Some("run-2"));
        assert_eq!(list.len(), 2, "a new editing session never overwrites the previous session's version");
        assert_eq!(list[1].size, "first run".len() as u64);
        // Within that session, writes join again.
        assert_eq!(write_in(&root, &file, "second run, more", VersionKind::Autosave, now + 180, now + 180, Some("run-2")).len(), 2);
        // A sessionless AutoSave entry (the state kept before a restore) never joins.
        assert_eq!(write_in(&root, &file, "kept", VersionKind::Autosave, now + 200, now + 200, None).len(), 3);
        assert_eq!(write_in(&root, &file, "after", VersionKind::Autosave, now + 210, now + 210, Some("run-2")).len(), 4);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn entry(n: usize, at: u64, kind: VersionKind) -> VersionInfo {
        VersionInfo {
            id: format!("{n:032x}"),
            saved_at: crate::cmd::iso_from_unix_secs(at),
            author: "Tester".into(),
            size: 1,
            kind,
            ..VersionInfo::default()
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn pruning_follows_the_automatic_schedule() {
        let root = temp("prune");
        let day = DAY_SECS;
        // Midnight, so hour, day and week boundaries are easy to place.
        let now: u64 = 1_800_000_000 / day * day;
        let mut entries = Vec::new();
        let mut n = 0usize;
        let mut add = |entries: &mut Vec<VersionInfo>, at: u64| {
            n += 1;
            entries.push(entry(n, at, VersionKind::Save));
            n
        };
        // Inside 30 days: every version, even seconds apart.
        let recent: Vec<usize> = (0..5).map(|i| add(&mut entries, now - 29 * day + i)).collect();
        // 30 to 60 days: one per hour (the newest of the hour).
        let hour_a_old = add(&mut entries, now - 40 * day + 60);
        let hour_a_new = add(&mut entries, now - 40 * day + 120);
        let hour_b = add(&mut entries, now - 40 * day + 3_600 + 60);
        // 60 to 180 days: one per day.
        let day_old = add(&mut entries, now - 100 * day + 3_600);
        let day_new = add(&mut entries, now - 100 * day + 7_200);
        let next_day = add(&mut entries, now - 99 * day + 3_600);
        // Beyond 180 days: one per week, and never removed for age alone.
        // The Monday that starts the week about 400 days back.
        let week = (((now - 400 * day) / day + 3) / 7 * 7 - 3) * day;
        let week_old = add(&mut entries, week + 3_600);
        let week_new = add(&mut entries, week + day);
        let ancient = add(&mut entries, now - 3_000 * day);
        let kept: BTreeSet<String> = prune(&root, entries, now).into_iter().map(|v| v.id).collect();
        let has = |n: usize| kept.contains(&format!("{n:032x}"));
        assert!(recent.iter().all(|&n| has(n)), "every version of the last 30 days stays");
        assert!(has(hour_a_new) && !has(hour_a_old) && has(hour_b), "one per hour between 30 and 60 days");
        assert!(has(day_new) && !has(day_old) && has(next_day), "one per day between 60 and 180 days");
        assert!(has(week_new) && !has(week_old), "one per week after 180 days");
        assert!(has(ancient), "a document left alone keeps its old history");

        // Count and size limits, newest first.
        let many: Vec<VersionInfo> = (0..MAX_VERSIONS + 20).map(|i| entry(i + 1, now - u64::try_from(i).unwrap(), VersionKind::Save)).collect();
        let kept = prune(&root, many, now);
        assert_eq!(kept.len(), MAX_VERSIONS);
        assert!(kept.iter().any(|v| v.id == format!("{:032x}", 1)), "the newest stays");
        let large: Vec<VersionInfo> =
            (0..100).map(|i| VersionInfo { size: 6 << 20, ..entry(i + 1, now - u64::try_from(i).unwrap(), VersionKind::Save) }).collect();
        let kept = prune(&root, large, now);
        assert!(kept.iter().map(|v| v.size).sum::<u64>() <= MAX_DOCUMENT_BYTES);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_first_original_is_pinned_past_every_limit() {
        let root = temp("pinned");
        let now: u64 = 1_800_000_000;
        let mut list = vec![VersionInfo { size: 10 << 20, ..entry(1, now - 5_000 * DAY_SECS, VersionKind::Original) }];
        list.extend(
            (0..MAX_VERSIONS + 10).map(|i| VersionInfo { size: 1 << 20, ..entry(i + 2, now - u64::try_from(i).unwrap(), VersionKind::Save) }),
        );
        let kept = prune(&root, list, now);
        assert!(kept.len() <= MAX_VERSIONS);
        assert!(kept.iter().map(|v| v.size).sum::<u64>() <= MAX_DOCUMENT_BYTES);
        assert_eq!(kept.last().map(|v| v.kind), Some(VersionKind::Original), "the original outlives the count and size limits");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_damaged_index_never_costs_the_stored_versions() {
        let root = temp("damaged");
        let file = root.join("note.docx");
        // Recovered entries are dated by their files, so this test runs on the real clock.
        let now = crate::cmd::now_unix();
        let _ = write(&root, &file, "one", VersionKind::Save, now, now);
        let list = write(&root, &file, "two", VersionKind::Save, now + 1, now + 1);
        let dir = document_dir(&root, &file);
        fs::write(dir.join(INDEX_FILE), b"{ not json").unwrap();
        let recovered = read_index(&dir);
        assert_eq!(recovered.len(), 2, "version files come back from a corrupt index");
        let after = write(&root, &file, "three", VersionKind::Save, now + 2, now + 2);
        assert_eq!(after.len(), 3, "the next save keeps the recovered versions instead of pruning them");
        assert!(list.iter().all(|v| dir.join(format!("{}.bin", v.id)).is_file()));
        // One bad entry costs only itself.
        let mut values: Vec<serde_json::Value> = after.iter().map(|v| serde_json::to_value(v).unwrap()).collect();
        values.push(json!({"id": 7, "kind": "future"}));
        fs::write(dir.join(INDEX_FILE), serde_json::to_vec(&values).unwrap()).unwrap();
        assert_eq!(read_index(&dir), after);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn damaged_version_bytes_are_refused() {
        let root = temp("checksum");
        let file = root.join("note.docx");
        let list = write(&root, &file, "good", VersionKind::Save, 1_800_000_000, 1_800_000_000);
        let id = list[0].id.clone();
        fs::write(document_dir(&root, &file).join(format!("{id}.bin")), b"evil").unwrap();
        let store = Versions::start(root.clone(), std::sync::Arc::new(|| {})).unwrap();
        assert!(store.read(&file, &id).is_err(), "same size, different bytes");
        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn autosave_over_a_file_without_history_keeps_its_original_bytes() {
        let root = temp("original");
        let file = root.join("Report.docx");
        crate::io::save_path(&file, &wordcraft_doc::Document::from_text("written elsewhere")).unwrap();
        let original = fs::read(&file).unwrap();
        let mut s = session_in(&root, "");
        s.run("file.open", &json!({"path": file.to_string_lossy()})).unwrap();
        s.run("document.setText", &json!({"text": "edited here"})).unwrap();
        assert_eq!(s.run("file.save", &json!({"ifUnchanged": true})).unwrap()["saved"], true);
        let list = wait_versions(&s, &file, 2);
        assert_eq!(list.len(), 2, "{list:?}");
        let first = list.iter().find(|v| v.kind == VersionKind::Original).unwrap();
        assert_eq!(version_bytes(&s, &file, &first.id), original, "the bytes that were there before the first overwrite");
        assert!(first.captured_at.is_some());
        let newest = &list[0];
        assert_eq!(newest.kind, VersionKind::Autosave);
        assert_eq!(newest.storage, VersionStorage::Parts);
        assert!(
            same_parts(&version_bytes(&s, &file, &newest.id), &fs::read(&file).unwrap()),
            "the version holds exactly the parts that went to disk"
        );
        // Later saves of the same opening do not read or store the original again.
        s.run("document.setText", &json!({"text": "edited again"})).unwrap();
        s.run("file.save", &json!({})).unwrap();
        let list = wait_versions(&s, &file, 3);
        assert_eq!(list.iter().filter(|v| v.kind == VersionKind::Original).count(), 1);
        assert!(same_parts(&version_bytes(&s, &file, &list[0].id), &fs::read(&file).unwrap()));
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn bytes_changed_by_another_program_are_kept_before_saving_over_them() {
        let root = temp("outside");
        let file = root.join("Notes.docx");
        let mut s = session_in(&root, "mine");
        s.run("file.save", &json!({"path": file.to_string_lossy()})).unwrap();
        assert_eq!(wait_versions(&s, &file, 1).len(), 1);
        crate::io::save_path(&file, &wordcraft_doc::Document::from_text("their edit")).unwrap();
        let theirs = fs::read(&file).unwrap();
        s.run("document.setText", &json!({"text": "mine, later"})).unwrap();
        // An explicit save (or Keep this version) writes over the other program's edit.
        s.run("file.save", &json!({})).unwrap();
        let list = wait_versions(&s, &file, 3);
        let kept = list.iter().find(|v| v.kind == VersionKind::Original).unwrap();
        assert_eq!(version_bytes(&s, &file, &kept.id), theirs);
        // Reopening a file changed while it was closed is protected the same way.
        crate::io::save_path(&file, &wordcraft_doc::Document::from_text("edited while closed")).unwrap();
        let closed = fs::read(&file).unwrap();
        s.run("file.open", &json!({"path": file.to_string_lossy()})).unwrap();
        s.run("document.setText", &json!({"text": "reopened"})).unwrap();
        s.run("file.save", &json!({"ifUnchanged": true})).unwrap();
        let list = wait_versions(&s, &file, 5);
        assert!(list.iter().any(|v| v.kind == VersionKind::Original && version_bytes(&s, &file, &v.id) == closed));
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_broken_history_never_stops_or_changes_the_save() {
        let root = temp("broken");
        let file = root.join("Safe.docx");
        crate::io::save_path(&file, &wordcraft_doc::Document::from_text("before")).unwrap();
        let mut s = session_in(&root, "");
        s.run("file.open", &json!({"path": file.to_string_lossy()})).unwrap();
        // The history folder becomes a plain file: every history write fails.
        let versions = root.join("versions");
        fs::remove_dir_all(&versions).unwrap();
        fs::write(&versions, b"not a folder").unwrap();
        s.run("document.setText", &json!({"text": "saved anyway"})).unwrap();
        let result = s.run("file.save", &json!({"ifUnchanged": true})).unwrap();
        assert_eq!(result["saved"], true);
        let mut reopened = crate::Session::new(wordcraft_doc::Document::new());
        reopened.run("file.open", &json!({"path": file.to_string_lossy()})).unwrap();
        assert_eq!(body(&reopened), "saved anyway");
        let mut reported = None;
        for _ in 0..500 {
            reported = s.versions_error();
            if reported.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(reported.is_some(), "the failure is reported, not hidden");
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn rename_takes_the_history_along_and_save_as_starts_its_own() {
        let root = temp("rename");
        let first = root.join("Draft.docx");
        let mut s = session_in(&root, "one");
        s.run("file.save", &json!({"path": first.to_string_lossy()})).unwrap();
        s.run("document.setText", &json!({"text": "two"})).unwrap();
        s.run("file.save", &json!({})).unwrap();
        assert_eq!(wait_versions(&s, &first, 2).len(), 2);
        let first_key = document_dir(&root.join("versions"), &first);

        s.run("file.rename", &json!({"name": "Final"})).unwrap();
        let renamed = root.join("Final.docx");
        assert!(renamed.is_file() && !first.exists());
        let list = wait_versions(&s, &renamed, 2);
        assert_eq!(list.len(), 2, "the history follows the rename");
        assert!(!first_key.exists(), "nothing is left under the old name");
        s.run("document.setText", &json!({"text": "three"})).unwrap();
        s.run("file.save", &json!({"ifUnchanged": true})).unwrap();
        assert_eq!(wait_versions(&s, &renamed, 3).len(), 3, "saving after a rename adds to the same history");

        // Save As makes a new file with a history of its own; the original keeps its history.
        let copy = root.join("Copy.docx");
        s.run("file.saveAs", &json!({"path": copy.to_string_lossy()})).unwrap();
        assert_eq!(wait_versions(&s, &copy, 1).len(), 1);
        assert_eq!(wait_versions(&s, &renamed, 3).len(), 3);
        // A copy in another format is an export, not a version.
        let text = root.join("Copy.txt");
        s.run("file.saveAs", &json!({"path": text.to_string_lossy()})).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!document_dir(&root.join("versions"), &text).exists());
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_file_moved_outside_the_app_finds_its_history_but_a_copy_does_not() {
        let root = temp("moved");
        let docs = root.join("docs");
        fs::create_dir_all(docs.join("archive")).unwrap();
        let original = docs.join("Plan.docx");
        let mut s = session_in(&root, "plan");
        s.run("file.save", &json!({"path": original.to_string_lossy()})).unwrap();
        assert_eq!(wait_versions(&s, &original, 1).len(), 1);

        // A copy while the original still exists: no history, like a copy in OneDrive.
        let copy = docs.join("Plan copy.docx");
        fs::copy(&original, &copy).unwrap();
        s.run("file.open", &json!({"path": copy.to_string_lossy()})).unwrap();
        s.run("document.setText", &json!({"text": "copy"})).unwrap();
        s.run("file.save", &json!({"ifUnchanged": true})).unwrap();
        let copy_list = wait_versions(&s, &copy, 2);
        assert_eq!(copy_list.iter().filter(|v| v.kind == VersionKind::Original).count(), 1, "the copy starts its own history");
        assert_eq!(wait_versions(&s, &original, 1).len(), 1, "the original keeps its history");

        // Moved in File Explorer: the history comes along at the next listing.
        let moved = docs.join("archive").join("Plan.docx");
        fs::rename(&original, &moved).unwrap();
        s.run("file.open", &json!({"path": moved.to_string_lossy()})).unwrap();
        let mut found = Vec::new();
        for _ in 0..500 {
            found = s.version_infos();
            if !found.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(found.len(), 1, "the moved file's history is found");
        assert!(!document_dir(&root.join("versions"), &original).exists());
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn persistent_open_and_restore_keep_the_document_separate_and_undoable() {
        let root = temp("open-restore");
        let file = root.join("note.txt");
        let list = write(&root, &file, "previous", VersionKind::Save, 1_800_000_000, 1_800_000_000);
        let id = list.first().map(|entry| entry.id.clone()).unwrap_or_default();

        let mut opened = crate::Session::new(wordcraft_doc::Document::from_text("current"));
        opened.path = Some(file.clone());
        opened.version_store = Some(Versions::start(root.clone(), std::sync::Arc::new(|| {})).unwrap());
        let result = opened.run("file.versions", &json!({"open": id}));
        assert!(result.is_ok());
        assert_eq!(body(&opened), "previous");
        assert!(opened.path.is_none());
        assert!(opened.file_name.as_deref().is_some_and(|name| name.contains("(version ")));

        let mut opened_restore = crate::Session::new(wordcraft_doc::Document::from_text("current"));
        opened_restore.path = Some(file.clone());
        opened_restore.version_store = Some(Versions::start(root.clone(), std::sync::Arc::new(|| {})).unwrap());
        assert!(opened_restore.run("file.versions", &json!({"open": id.clone()})).is_ok());
        assert!(opened_restore.run("file.versions", &json!({"restore": id.clone()})).is_ok());
        assert_eq!(body(&opened_restore), "previous");
        assert!(opened_restore.run("edit.undo", &json!({})).is_ok());
        assert_eq!(body(&opened_restore), "current");

        let mut restored = crate::Session::new(wordcraft_doc::Document::from_text("current"));
        restored.path = Some(file);
        restored.version_store = Some(Versions::start(root.clone(), std::sync::Arc::new(|| {})).unwrap());
        assert!(restored.run("file.versions", &json!({"restore": list[0].id.clone()})).is_ok());
        assert_eq!(body(&restored), "previous");
        assert!(restored.run("edit.undo", &json!({})).is_ok());
        assert_eq!(body(&restored), "current");
        drop(opened);
        drop(opened_restore);
        drop(restored);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn restore_records_a_version_at_once_and_keeps_the_unsaved_state_before_it() {
        let root = temp("restore-now");
        let file = root.join("Letter.docx");
        let mut s = session_in(&root, "first draft");
        s.run("file.save", &json!({"path": file.to_string_lossy()})).unwrap();
        let first = wait_versions(&s, &file, 1);
        s.run("document.setText", &json!({"text": "unsaved work"})).unwrap();
        assert!(s.dirty);
        // AutoSave off or not yet run: nothing writes the file before the restore.
        s.run("file.versions", &json!({"restore": first[0].id.clone()})).unwrap();
        assert_eq!(body(&s), "first draft");
        let list = wait_versions(&s, &file, 3);
        assert_eq!(list.len(), 3, "{list:?}");
        assert_eq!(list[0].kind, VersionKind::Restore, "the restore is the newest version straight away");
        assert!(same_parts(&version_bytes(&s, &file, &list[0].id), &version_bytes(&s, &file, &first[0].id)), "with the restored content");
        let kept = &list[1];
        let mut check = crate::Session::new(wordcraft_doc::Document::new());
        let (doc, _) = crate::io::open_bytes_with("Letter.docx", &version_bytes(&s, &file, &kept.id), None).unwrap();
        check.set_document(doc);
        assert_eq!(body(&check), "unsaved work", "the state before the restore stays in the history");
        // Still one Undo step.
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(body(&s), "unsaved work");
        // A later AutoSave is an ordinary version, not a second restore.
        s.run("file.versions", &json!({"restore": first[0].id.clone()})).unwrap();
        s.run("file.save", &json!({"ifUnchanged": true})).unwrap();
        let list = wait_versions(&s, &file, 6);
        assert_eq!(list[0].kind, VersionKind::Autosave);
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_version_parameters_return_errors() {
        let mut session = crate::Session::new(wordcraft_doc::Document::new());
        assert!(session.run("file.versions", &json!(null)).is_err());
        assert!(session.run("file.versions", &json!({"unknown": true})).is_err());
        assert!(session.run("file.versions", &json!({"open": 7})).is_err());
        assert!(session.run("file.versions", &json!({"restore": []})).is_err());
    }

    #[test]
    fn legacy_restore_is_one_undo_step() {
        let mut session = crate::Session::new(wordcraft_doc::Document::from_text("old"));
        session.run("file.versions", &json!({"save": "old"})).unwrap();
        session.run("document.setText", &json!({"text": "current"})).unwrap();
        session.run("file.versions", &json!({"restore": 0})).unwrap();
        assert_eq!(session.doc.plain_text(wordcraft_doc::StoryRef::Body), "old");
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(session.doc.plain_text(wordcraft_doc::StoryRef::Body), "current");
    }
}
