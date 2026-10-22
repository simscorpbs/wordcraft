//! Local version history for desktop documents.
//!
//! The service backed histories of iCloud Drive, Google Drive and OneDrive remain owned by
//! those services.  This module is the small local safety net WordCraft keeps in the profile:
//! document bytes are written by a worker, beside a bounded JSON index.
//!
//! Storage, per document, under `<root>/<key>/`, where the key is the document's random id
//! (Word packages, see [`meta`]) or a hash of its canonical path (other files):
//! - `<id>.parts.json` and `objects/<sha256>`: a version of a zip package (`.docx`, `.odt`…)
//!   kept as its parts, each part an object named by the hash of its content, so what did not
//!   change between versions is stored once and what changed is a small delta (see [`pack`]);
//! - `<id>.bin`: the exact bytes of one version, for the `original` copy, files that are not a
//!   zip, and versions written by earlier builds; verified after writing;
//! - `<id>.meta.json`: the metadata of a whole version (a manifest carries its own);
//! - `gc/`: dated marks of objects no version needs, deleted after a grace period;
//! - `document.json` (folders kept by path): the path the folder last belonged to, so a file
//!   moved outside WordCraft can find its history again (see `adopt_moved`).
//!
//! Nothing a version writes is ever rewritten, so a history folder can be one a cloud app
//! syncs between two computers: versions and objects have unique names, the list of versions
//! is rebuilt from their own files, and an object is deleted only after a week unneeded. One or
//! two history folders can be used (File › Options › Save); every write goes to all of them and
//! they are reconciled when one comes back.
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
    /// history yet (a `.docx` made before WordCraft, a file Save As replaced).
    Original,
    /// The bytes another program or computer wrote to the file, found on disk when WordCraft
    /// was about to write over them.
    External,
    /// The first version of a file made with Save As (or Save to Cloud) from another document.
    #[serde(rename = "saveAs")]
    SaveAs,
    /// The first version of the copy Keep both makes after a save conflict.
    #[serde(rename = "conflictCopy")]
    ConflictCopy,
}

impl VersionKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::Autosave => "autosave",
            Self::Restore => "restore",
            Self::Original => "original",
            Self::External => "external",
            Self::SaveAs => "saveAs",
            Self::ConflictCopy => "conflictCopy",
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
    /// An AutoSave version that replaced this one in the same window: the replaced version is
    /// no longer listed. (Records are never rewritten, so a later write supersedes instead.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    /// The document's random id (`WordCraftDocumentId` in its custom properties) when its
    /// history is kept by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    /// Where the version comes from and what it holds (see [`VersionDetails`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<VersionDetails>>,
    /// The order versions were written in within one second (`savedAt` has whole seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written: Option<u64>,
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
pub(crate) mod meta;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod pack;

/// The device label versions record when the user has not chosen one: the system's name only
/// ("Windows", "Mac"), never the computer's own name.
pub fn default_device_label() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "Mac",
        "linux" => "Linux",
        "freebsd" | "openbsd" | "netbsd" | "dragonfly" => "BSD",
        _ => "Computer",
    }
}

/// The id a document carries in its `docProps/custom.xml` (see [`meta::DOC_ID_PROP`]), when it
/// is a valid one.
pub fn document_uuid(doc: &wordcraft_doc::Document) -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        doc.custom_prop(meta::DOC_ID_PROP).map(str::trim).filter(|id| meta::document_id_ok(id)).map(str::to_owned)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = doc;
        None
    }
}

/// Legacy, read only: the one index file earlier builds rewrote on every save. Two computers
/// syncing a folder would both rewrite it, so the history no longer keeps one: the list is
/// rebuilt from the versions' own immutable files (and cached in memory).
#[cfg(not(target_arch = "wasm32"))]
const INDEX_FILE: &str = "index.json";
#[cfg(not(target_arch = "wasm32"))]
const PARTS_SUFFIX: &str = ".parts.json";
/// The metadata of a version kept whole (`<id>.bin`), written once beside it.
#[cfg(not(target_arch = "wasm32"))]
const META_SUFFIX: &str = ".meta.json";
#[cfg(not(target_arch = "wasm32"))]
const OBJECTS_DIR: &str = "objects";
/// Marks of objects no version needed when last looked at: `<sha256>.<unix seconds>`.
#[cfg(not(target_arch = "wasm32"))]
const GC_DIR: &str = "gc";
#[cfg(not(target_arch = "wasm32"))]
const MAX_MANIFEST_BYTES: u64 = 8 << 20;
/// Largest object file read: a whole part compressed (incompressible data grows a little).
#[cfg(not(target_arch = "wasm32"))]
const MAX_OBJECT_FILE: u64 = pack::MAX_PART_BYTES + (16 << 20);
/// The identity record of a folder kept by path (files that carry no document id).
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
/// An object no kept version needs is deleted only once it has been unneeded this long. In a
/// folder two computers sync, one computer may see a new object before the manifest that uses
/// it arrives; the wait keeps it from deleting what the other computer just wrote.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const ORPHAN_GRACE_SECS: u64 = 7 * DAY_SECS;
#[cfg(not(target_arch = "wasm32"))]
const MAX_ID_LEN: usize = 32;
/// Most per-document folders looked at when a file with no history may have been moved.
#[cfg(not(target_arch = "wasm32"))]
const MAX_BUCKET_SCAN: usize = 20_000;
/// Most files looked at in one document's folder.
#[cfg(not(target_arch = "wasm32"))]
const MAX_RECOVERED_FILES: usize = 30_000;
/// Version history folders: the settings folder, another folder, or both.
pub const MAX_HISTORY_FOLDERS: usize = 2;

/// Another version this one came from: the document a copy or Save As was made of.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
}

/// The file as it was on disk just before the save that made this version replaced it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiskBefore {
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Counts about the document at this version: never any of its text.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionStats {
    pub words: u64,
    pub characters: u64,
    pub paragraphs: u64,
    /// When a layout was at hand (none is made just to count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<u64>,
    pub comments: u64,
    pub tracked_changes: u64,
    /// Words inserted and removed since the previous version of this editing session, counted
    /// by word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words_inserted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words_removed: Option<u64>,
}

/// The document's own properties (`docProps/core.xml`) at this version.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionProperties {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub creator: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_modified_by: String,
    #[serde(default)]
    pub revision: u64,
}

/// Where a version comes from and what it holds, recorded in its manifest. Nothing here names
/// the computer or the account: the device is a random installation id and a label the user
/// can change, and the path is relative to the user's or the provider's folder.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct VersionDetails {
    /// The version before this one in the document's history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<VersionOrigin>,
    /// Parts of the package that differ from the parent version (all of them for the first).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_parts: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed_parts: Vec<String>,
    /// The local time zone's offset from UTC when it was saved, in seconds.
    pub utc_offset: i32,
    /// When the editing session (this opening of the document) began, and how long it had run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editing_seconds: Option<u64>,
    /// What made the save: `save`, `autosave`, `close`, `restore:<version>`, `saveAs`, `copy`,
    /// `beforeSave` (the file kept before WordCraft wrote over it), `beforeRestore`.
    pub trigger: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub initials: String,
    /// This installation's random id and the device label (Options › Save).
    pub installation: String,
    pub device: String,
    pub os: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub os_version: String,
    pub app_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// `~/Documents/x.docx`, `onedrive:/Folder/x.docx`, or a path outside both.
    pub path: String,
    /// `local`, `icloud`, `onedrive` or `gdrive`.
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<DiskBefore>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<VersionProperties>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<VersionStats>,
}

/// What `file.save` worked out before writing, for the version recorded after it.
#[derive(Clone, Debug, Default)]
pub(crate) struct PendingVersion {
    pub kind: Option<VersionKind>,
    pub origin: Option<VersionOrigin>,
    pub trigger: String,
    pub before: Option<DiskBefore>,
    pub document_id: Option<String>,
    pub path: PathBuf,
}

/// Which history `file.versions={"delete": …}` removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteScope {
    Document,
    All,
}

/// What happens to the history already kept when the history folders change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relocation {
    Move,
    Copy,
    Leave,
}

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
    /// The document's id: its history is kept under it. `None` keeps it by path.
    document_id: Option<String>,
    details: VersionDetails,
    cloud: Vec<wordcraft_cloud::CloudRoot>,
}

#[cfg(not(target_arch = "wasm32"))]
enum Job {
    Save(Box<VersionWrite>),
    /// List the versions of a document. With `probe`, a file kept by path and without history
    /// may look for the history of a file moved outside WordCraft (it reads the file).
    List {
        path: PathBuf,
        document_id: Option<String>,
        probe: bool,
        cloud: Vec<wordcraft_cloud::CloudRoot>,
    },
    /// `file.rename` moved a document kept by path: its history follows it.
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    /// The history folders changed: copy (and with `keep_old` false, move) what the old ones
    /// hold into the new ones.
    Relocate {
        from: Vec<PathBuf>,
        to: Vec<PathBuf>,
        keep_old: bool,
    },
}

#[cfg(not(target_arch = "wasm32"))]
struct Event {
    path: PathBuf,
    list: Vec<VersionInfo>,
    result: Result<(), String>,
}

/// The identity record of a folder kept by path.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize, Serialize)]
struct DocumentMeta {
    path: String,
}

/// A version kept whole: its metadata, written once beside `<id>.bin`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize, Serialize)]
struct WholeRecord {
    format: u32,
    info: VersionInfo,
}

/// What the worker and the synchronous history writes share, under one lock.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Shared {
    /// The history folders: the first is written first, all are written and read.
    roots: Vec<PathBuf>,
    /// Folders the last write could not reach (unplugged, no permission). They are brought up
    /// to date when they can be reached again.
    unavailable: Vec<PathBuf>,
    relocating: bool,
}

/// Desktop worker state.  Nothing in this struct is shared with the worker by reference, so a
/// slow disk never holds a borrow of the editing session.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Versions {
    tx: mpsc::SyncSender<Job>,
    rx: mpsc::Receiver<Event>,
    /// The settings folder's history folder, the default.
    default_root: PathBuf,
    shared: Arc<Mutex<Shared>>,
    /// Random per run of the app: with the session's document id it names an editing session.
    run_id: String,
    /// This installation's random id (see [`meta::installation_id`]).
    installation: String,
    /// `(document id, canonical path)` whose bytes on disk are known to be in the history for
    /// the document open now. Reset by opening another document (its id changes).
    verified: Option<(u64, PathBuf)>,
    /// `(document id, canonical path)` whose document id was checked against copies.
    identity_checked: Option<(u64, PathBuf)>,
    /// The words of the document at its last version this session, to count an edit's words.
    bag: Option<(u64, meta::WordBag)>,
    pub list: Vec<VersionInfo>,
    pub ready: bool,
    pub busy: bool,
    pub error: Option<String>,
    current: Option<(PathBuf, Option<String>)>,
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

/// A document's folder name: 16 hex digits for a folder kept by path, 32 for a document id.
#[cfg(not(target_arch = "wasm32"))]
fn key_ok(name: &str) -> bool {
    matches!(name.len(), 16 | 32) && name.bytes().all(|b| b.is_ascii_hexdigit())
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

/// Two paths that name the same file (case aside on Windows).
#[cfg(not(target_arch = "wasm32"))]
fn same_file_path(a: &Path, b: &Path) -> bool {
    let (a, b) = (canonical_path(a), canonical_path(b));
    if cfg!(windows) { a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase() } else { a == b }
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

/// The history folder of the document at `path` kept by path. A folder an earlier build made
/// under the case-sensitive key is moved to the current key the first time it is looked up; if
/// the move fails it is used where it is, so nothing is hidden.
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

/// The folder in `root` that holds a document's history: by its id, else by its path.
#[cfg(not(target_arch = "wasm32"))]
fn bucket_dir(root: &Path, path: &Path, document_id: Option<&str>) -> PathBuf {
    match document_id {
        Some(id) => root.join(meta::id_bucket(id)),
        None => document_dir(root, path),
    }
}

/// The document's folder in every history folder that can be reached now, and the history
/// folders that cannot.
#[cfg(not(target_arch = "wasm32"))]
fn bucket_dirs(roots: &[PathBuf], path: &Path, document_id: Option<&str>) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut dirs = Vec::new();
    let mut unavailable = Vec::new();
    for root in roots {
        if private_dir(root).is_ok() {
            dirs.push(bucket_dir(root, path, document_id));
        } else {
            unavailable.push(root.clone());
        }
    }
    (dirs, unavailable)
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_iso_secs(value: &str) -> Option<u64> {
    parse_iso_timestamp(value)
}

#[cfg(not(target_arch = "wasm32"))]
fn modified_secs(meta: &fs::Metadata) -> Option<u64> {
    meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Newest first; within one second, the one written last first.
#[cfg(not(target_arch = "wasm32"))]
fn sort_newest_first(list: &mut [VersionInfo]) {
    list.sort_by_key(|v| std::cmp::Reverse((parse_iso_secs(&v.saved_at).unwrap_or(0), v.written.unwrap_or(0))));
}

/// A number that grows with every version this process writes, within one second.
#[cfg(not(target_arch = "wasm32"))]
fn write_order(now: u64) -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    now.saturating_mul(1_000_000).saturating_add(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 1_000_000)
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

/// Parsed version records, by file: they never change once written, so a file seen with the
/// same size and date is not parsed again. Bounded; cleared when full.
#[cfg(not(target_arch = "wasm32"))]
fn record_cache() -> &'static Mutex<std::collections::HashMap<PathBuf, (u64, Option<std::time::SystemTime>, VersionInfo)>> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<PathBuf, (u64, Option<std::time::SystemTime>, VersionInfo)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[cfg(not(target_arch = "wasm32"))]
fn cached_record(path: &Path, parse: impl FnOnce() -> Option<VersionInfo>) -> Option<VersionInfo> {
    let meta = fs::symlink_metadata(path).ok()?;
    let stamp = (meta.len(), meta.modified().ok());
    let cache = record_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((len, modified, info)) = cache.get(path)
        && (*len, *modified) == stamp
    {
        return Some(info.clone());
    }
    drop(cache);
    let info = parse()?;
    let mut cache = record_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() > 50_000 {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), (stamp.0, stamp.1, info.clone()));
    Some(info)
}

#[cfg(not(target_arch = "wasm32"))]
fn info_ok(v: &VersionInfo) -> bool {
    id_ok(&v.id) && v.size <= MAX_DOCUMENT_BYTES && parse_iso_secs(&v.saved_at).is_some()
}

/// Every version recorded in one document folder, newest first, superseded ones included.
/// Each version describes itself in its own file (a manifest, or the metadata beside a whole
/// copy); the index earlier builds kept is still read. Files that are not ours, half-synced or
/// named like a sync app's conflict copy (`x (1).parts.json`) are ignored, never deleted, and a
/// version file nothing describes comes back as a `save` dated by its file.
#[cfg(not(target_arch = "wasm32"))]
fn read_index(dir: &Path) -> Vec<VersionInfo> {
    let Ok(dir_meta) = fs::symlink_metadata(dir) else { return Vec::new() };
    if !dir_meta.is_dir() {
        return Vec::new();
    }
    let mut found: std::collections::BTreeMap<String, VersionInfo> = std::collections::BTreeMap::new();
    let mut bins: Vec<(String, fs::Metadata)> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten().take(MAX_RECOVERED_FILES) {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(id) = name.strip_suffix(PARTS_SUFFIX).filter(|id| id_ok(id)) {
                if let Some(manifest) = cached_record(&entry.path(), || read_manifest(dir, id).map(|m| m.info)) {
                    found.insert(id.to_owned(), VersionInfo { storage: VersionStorage::Parts, ..manifest });
                }
            } else if let Some(id) = name.strip_suffix(META_SUFFIX).filter(|id| id_ok(id)) {
                let record = cached_record(&entry.path(), || read_whole_record(dir, id));
                if let Some(info) = record
                    && fs::symlink_metadata(dir.join(format!("{id}.bin"))).is_ok_and(|m| m.is_file() && m.len() == info.size)
                {
                    found.entry(id.to_owned()).or_insert(VersionInfo { storage: VersionStorage::Whole, ..info });
                }
            } else if let Some(id) = name.strip_suffix(".bin").filter(|id| id_ok(id))
                && let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                bins.push((id.to_owned(), meta));
            }
        }
    }
    // The index of earlier builds: read, never written again.
    for v in legacy_index(dir) {
        if found.contains_key(&v.id) {
            continue;
        }
        let exists = match v.storage {
            VersionStorage::Whole => fs::symlink_metadata(dir.join(format!("{}.bin", v.id))).is_ok_and(|m| m.is_file() && m.len() == v.size),
            VersionStorage::Parts => false,
        };
        if exists {
            found.insert(v.id.clone(), v);
        }
    }
    for (id, meta) in bins {
        if found.contains_key(&id) || meta.len() > MAX_DOCUMENT_BYTES {
            continue;
        }
        found.insert(
            id.clone(),
            VersionInfo {
                id,
                saved_at: crate::cmd::iso_from_unix_secs(modified_secs(&meta).unwrap_or(0)),
                size: meta.len(),
                kind: VersionKind::Save,
                ..VersionInfo::default()
            },
        );
    }
    let mut list: Vec<VersionInfo> = found.into_values().filter(info_ok).collect();
    sort_newest_first(&mut list);
    list
}

/// The versions a person sees: those no AutoSave write in the same window replaced.
#[cfg(not(target_arch = "wasm32"))]
fn visible(list: &[VersionInfo]) -> Vec<VersionInfo> {
    let superseded: BTreeSet<&str> = list.iter().filter_map(|v| v.supersedes.as_deref()).collect();
    list.iter().filter(|v| !superseded.contains(v.id.as_str())).cloned().collect()
}

/// The versions of a document across all its folders (a mirror may hold versions the first
/// folder does not have yet), newest first.
#[cfg(not(target_arch = "wasm32"))]
fn read_union(dirs: &[PathBuf]) -> Vec<VersionInfo> {
    let mut seen = BTreeSet::new();
    let mut all = Vec::new();
    for dir in dirs {
        for v in read_index(dir) {
            if seen.insert(v.id.clone()) {
                all.push(v);
            }
        }
    }
    sort_newest_first(&mut all);
    all
}

#[cfg(not(target_arch = "wasm32"))]
fn legacy_index(dir: &Path) -> Vec<VersionInfo> {
    read_small(&dir.join(INDEX_FILE), MAX_INDEX_BYTES)
        .and_then(|bytes| serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| serde_json::from_value::<VersionInfo>(value).ok())
        .filter(info_ok)
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn read_whole_record(dir: &Path, id: &str) -> Option<VersionInfo> {
    let bytes = read_small(&dir.join(format!("{id}{META_SUFFIX}")), MAX_META_BYTES)?;
    let record: WholeRecord = serde_json::from_slice(&bytes).ok()?;
    (record.format == 2 && record.info.id == id).then_some(record.info)
}

/// Rewrite an earlier build's folder in the current layout: the metadata of every whole version
/// moves from the shared `index.json` into its own file, then the index goes.
#[cfg(not(target_arch = "wasm32"))]
fn migrate_layout(dir: &Path) {
    let legacy = legacy_index(dir);
    if fs::symlink_metadata(dir.join(INDEX_FILE)).is_err() {
        return;
    }
    let mut complete = true;
    for v in legacy.iter().filter(|v| v.storage == VersionStorage::Whole) {
        let bin = dir.join(format!("{}.bin", v.id));
        if !bin.is_file() || dir.join(format!("{}{META_SUFFIX}", v.id)).is_file() {
            continue;
        }
        let record = WholeRecord { format: 2, info: v.clone() };
        let written = serde_json::to_vec_pretty(&record)
            .map_err(|e| e.to_string())
            .and_then(|json| write_atomic(&dir.join(format!("{}{META_SUFFIX}", v.id)), &json));
        complete &= written.is_ok();
    }
    if complete {
        let _ = fs::remove_file(dir.join(INDEX_FILE));
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn read_meta(dir: &Path) -> Option<DocumentMeta> {
    let bytes = read_small(&dir.join(META_FILE), MAX_META_BYTES)?;
    serde_json::from_slice(&bytes).ok()
}

/// Record which file a folder kept by path belongs to (only when it changed), as a path
/// without the account name.
#[cfg(not(target_arch = "wasm32"))]
fn write_meta(dir: &Path, path: &Path, cloud: &[wordcraft_cloud::CloudRoot]) -> Result<(), String> {
    let text = meta::portable_path(path, cloud);
    if read_meta(dir).is_some_and(|m| m.path == text) {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&DocumentMeta { path: text }).map_err(|e| e.to_string())?;
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

/// Copy `source` to `target` so that `target` appears whole or not at all.
#[cfg(not(target_arch = "wasm32"))]
fn copy_atomic(source: &Path, target: &Path) -> Result<(), String> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    private_dir(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!(".{}.{}.tmp", target.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(), new_id()?));
    let copied = fs::copy(source, &tmp).map_err(|e| e.to_string());
    let expected = fs::symlink_metadata(source).map(|m| m.len()).map_err(|e| e.to_string());
    let result = match (copied, expected) {
        (Ok(n), Ok(len)) if n == len => fs::rename(&tmp, target).map_err(|e| e.to_string()),
        (Err(e), _) | (_, Err(e)) => Err(e),
        _ => Err("copy was short".into()),
    };
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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

/// The content of object `hash`, looked for in each of `dirs` (a document's folders): its chain
/// of deltas is followed back to a whole copy, and every step is checked against its own name,
/// so a damaged object anywhere in the chain is an error for the versions that use it and
/// nothing else.
#[cfg(not(target_arch = "wasm32"))]
fn load_object(dirs: &[PathBuf], hash: &str) -> Result<Vec<u8>, String> {
    let mut chain: Vec<(String, Vec<u8>)> = Vec::new();
    let mut current = hash.to_owned();
    loop {
        if !pack::hash_ok(&current) || chain.len() >= pack::MAX_CHAIN {
            return Err("a part of this version is damaged".into());
        }
        let bytes =
            dirs.iter().find_map(|dir| read_small(&object_path(dir, &current), MAX_OBJECT_FILE)).ok_or("a part of this version is missing")?;
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

/// Store the parts of a version as objects in `dir`, each a delta against the same part of
/// `base` when that pays. An object already there is reused only when it still reads back
/// correctly; a damaged one is written again whole, which also repairs every version that uses
/// it (an object's name is its content, so rewriting it changes nothing for anyone else).
/// Returns the bytes newly written.
#[cfg(not(target_arch = "wasm32"))]
fn store_parts(dir: &Path, parts: &[pack::Part], base: Option<&pack::Manifest>) -> Result<u64, String> {
    private_dir(&dir.join(OBJECTS_DIR)).map_err(|e| e.to_string())?;
    let here = [dir.to_path_buf()];
    let mut written = 0u64;
    for part in parts.iter().filter(|part| !part.meta.dir) {
        let hash = part.meta.sha256.as_str();
        let path = object_path(dir, hash);
        let object = if fs::symlink_metadata(&path).is_ok() {
            if load_object(&here, hash).is_ok() {
                continue;
            }
            pack::encode_full(&part.data)
        } else {
            let base_part = base.and_then(|manifest| manifest.parts.iter().find(|p| !p.dir && p.name == part.meta.name && p.sha256 != hash));
            let base = base_part.and_then(|p| {
                let depth = object_header(dir, &p.sha256).ok()?.0.depth;
                Some((p.sha256.clone(), depth, load_object(&here, &p.sha256).ok()?))
            });
            pack::encode(&part.data, base.as_ref().map(|(hash, depth, content)| (hash.as_str(), *depth, content.as_slice())))
        };
        write_atomic(&path, &object)?;
        if let Err(error) = load_object(&here, hash) {
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

/// Which parts changed from `parent` (all of them without one) and which went.
#[cfg(not(target_arch = "wasm32"))]
fn part_changes(parts: &[pack::ManifestPart], parent: Option<&pack::Manifest>) -> (Vec<String>, Vec<String>) {
    let Some(parent) = parent else { return (parts.iter().filter(|p| !p.dir).map(|p| p.name.clone()).collect(), Vec::new()) };
    let before: std::collections::HashMap<&str, &str> = parent.parts.iter().map(|p| (p.name.as_str(), p.sha256.as_str())).collect();
    let now: BTreeSet<&str> = parts.iter().map(|p| p.name.as_str()).collect();
    let changed = parts.iter().filter(|p| !p.dir && before.get(p.name.as_str()) != Some(&p.sha256.as_str())).map(|p| p.name.clone()).collect();
    let removed = parent.parts.iter().filter(|p| !now.contains(p.name.as_str())).map(|p| p.name.clone()).collect();
    (changed, removed)
}

/// Keep `bytes` (a zip package) as parts for version `info`: objects first, then the manifest,
/// which is never rewritten. The archive is rebuilt in memory and split again before anything is
/// written, so a version stored as parts is known to come back with the same parts. Returns the
/// space it added.
#[cfg(not(target_arch = "wasm32"))]
fn write_parts_version(
    dir: &Path,
    info: &VersionInfo,
    bytes: &[u8],
    base: Option<&pack::Manifest>,
    parent: Option<&pack::Manifest>,
) -> Result<u64, String> {
    let (parts, comment) = pack::split_zip(bytes).ok_or("not a zip package that can be kept as parts")?;
    let metas: Vec<pack::ManifestPart> = parts.iter().map(|part| part.meta.clone()).collect();
    let mut info = VersionInfo { storage: VersionStorage::Parts, stored: None, ..info.clone() };
    if let Some(details) = info.details.as_mut() {
        let (changed, removed) = part_changes(&metas, parent);
        details.changed_parts = changed;
        details.removed_parts = removed;
    }
    let manifest = pack::Manifest { format: 1, info, comment, parts: metas };
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
    write_atomic(&manifest_path(dir, &manifest.info.id), &json)?;
    Ok(added.saturating_add(json.len() as u64))
}

/// Keep `bytes` whole for version `info`: the bytes, then their metadata.
#[cfg(not(target_arch = "wasm32"))]
fn write_whole_version(dir: &Path, info: &VersionInfo, bytes: &[u8]) -> Result<u64, String> {
    write_version_file(dir, &info.id, bytes)?;
    let record = WholeRecord { format: 2, info: VersionInfo { storage: VersionStorage::Whole, ..info.clone() } };
    let json = serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?;
    if let Err(error) = write_atomic(&dir.join(format!("{}{META_SUFFIX}", info.id)), &json) {
        let _ = fs::remove_file(dir.join(format!("{}.bin", info.id)));
        return Err(error);
    }
    Ok(bytes.len() as u64)
}

/// The bytes of a version kept as parts: the archive rebuilt from its manifest. Every part is
/// checked against its hash; the zip around them may differ from the saved file's bytes.
#[cfg(not(target_arch = "wasm32"))]
fn read_parts_version(dirs: &[PathBuf], id: &str) -> Result<Vec<u8>, String> {
    let manifest = dirs.iter().find_map(|dir| read_manifest(dir, id)).ok_or("this version's list of parts is missing or damaged")?;
    let mut contents = Vec::with_capacity(manifest.parts.len());
    for part in &manifest.parts {
        if part.dir {
            contents.push(Vec::new());
            continue;
        }
        let content = load_object(dirs, &part.sha256)?;
        if content.len() as u64 != part.size {
            return Err("a part of this version is damaged".into());
        }
        contents.push(content);
    }
    pack::assemble_zip(&manifest, &contents)
}

/// The bytes of a version kept whole, from whichever folder has them.
#[cfg(not(target_arch = "wasm32"))]
fn read_whole_version(dirs: &[PathBuf], info: &VersionInfo) -> Result<Vec<u8>, String> {
    let mut last = "version bytes are missing".to_string();
    for dir in dirs {
        let file_path = dir.join(format!("{}.bin", info.id));
        let Ok(meta) = fs::symlink_metadata(&file_path) else { continue };
        if !meta.is_file() || meta.len() != info.size || meta.len() > MAX_DOCUMENT_BYTES {
            last = "version bytes are missing or invalid".into();
            continue;
        }
        let Some(bytes) = read_small(&file_path, MAX_DOCUMENT_BYTES) else { continue };
        if info.digest.as_deref().is_some_and(|digest| digest != digest_hex(&bytes)) {
            last = "version bytes are damaged (their checksum does not match)".into();
            continue;
        }
        return Ok(bytes);
    }
    Err(last)
}

/// Versions written whole by earlier builds move to parts a little at a time: at most one per
/// save, newest first, and only after the parts read back. The new manifest has the version's
/// id, so the version keeps its identity; the whole copy goes after.
#[cfg(not(target_arch = "wasm32"))]
fn migrate_one(dir: &Path, list: &[VersionInfo]) {
    let base = newest_manifest(dir, list);
    let Some(entry) = list.iter().find(|v| v.storage == VersionStorage::Whole && v.stored.is_none() && v.kind != VersionKind::Original) else {
        return;
    };
    let bin = dir.join(format!("{}.bin", entry.id));
    let Some(bytes) = read_small(&bin, MAX_DOCUMENT_BYTES) else { return };
    let digest = digest_hex(&bytes);
    let mut info = entry.clone();
    let convertible = bytes.starts_with(b"PK\x03\x04") && info.digest.as_ref().is_none_or(|known| *known == digest);
    info.digest = Some(digest);
    if convertible && let Ok(stored) = write_parts_version(dir, &info, &bytes, base.as_ref(), None) {
        let _ = stored;
        let _ = fs::remove_file(&bin);
        let _ = fs::remove_file(dir.join(format!("{}{META_SUFFIX}", entry.id)));
        return;
    }
    // Not tried again: the metadata now says how much it takes.
    let record = WholeRecord { format: 2, info: VersionInfo { stored: Some(entry.size), ..info } };
    if let Ok(json) = serde_json::to_vec_pretty(&record) {
        let _ = write_atomic(&dir.join(format!("{}{META_SUFFIX}", entry.id)), &json);
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
    /// its own files.
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

/// Delete the files of versions `ids` in `dir`.
#[cfg(not(target_arch = "wasm32"))]
fn remove_versions(dir: &Path, ids: &BTreeSet<String>) {
    for id in ids.iter().filter(|id| id_ok(id)) {
        for name in [format!("{id}.bin"), format!("{id}{META_SUFFIX}"), format!("{id}{PARTS_SUFFIX}")] {
            let _ = fs::remove_file(dir.join(name));
        }
    }
}

/// Apply the retention rules to every version in `all` (superseded ones included) and delete
/// the files of the versions that go. The decision depends only on the versions' own records,
/// so two computers sharing a folder make the same one. Returns what is kept (newest first) and
/// the ids removed:
/// - versions an AutoSave write in the same window replaced go;
/// - every version kept for [`KEEP_ALL_SECS`], then the newest of each hour until
///   [`KEEP_HOURLY_SECS`], of each day until [`KEEP_DAILY_SECS`] and of each week after that;
/// - at most [`MAX_VERSIONS`] versions and [`MAX_DOCUMENT_BYTES`] of real disk space per
///   document (a part shared by several versions counts once), newest first;
/// - the oldest `original` (the file as it was before WordCraft first wrote it) is pinned: it
///   outlives the time rules and is the last to go for the count and size limits. This goes
///   beyond OneDrive on purpose: AutoSave is on for local files that never had a server-side
///   history, so this copy is the only way back to what was there.
///
/// Objects are not touched here; [`collect_objects`] does that, with a grace period.
#[cfg(not(target_arch = "wasm32"))]
fn prune(dir: &Path, all: Vec<VersionInfo>, now: u64) -> (Vec<VersionInfo>, BTreeSet<String>) {
    let mut list = visible(&all);
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
    let keep: BTreeSet<&str> = kept.iter().map(|v| v.id.as_str()).collect();
    let removed: BTreeSet<String> = all.iter().filter(|v| !keep.contains(v.id.as_str())).map(|v| v.id.clone()).collect();
    remove_versions(dir, &removed);
    (kept, removed)
}

/// Delete the objects of `dir` no kept version needs, directly or as the base of a delta, once
/// they have been unneeded for [`ORPHAN_GRACE_SECS`]. The first time an object is found
/// unneeded, a dated mark is left in `gc/`; a mark goes again when a version needs the object.
/// When anything could not be read, nothing is marked or deleted: an unreadable manifest must
/// not cost its parts.
#[cfg(not(target_arch = "wasm32"))]
fn collect_objects(dir: &Path, kept: &[VersionInfo], now: u64) {
    let mut usage = Usage::new(dir);
    let mut needed = BTreeSet::new();
    for entry in kept {
        needed.extend(usage.needs(entry).0);
    }
    if usage.incomplete {
        return;
    }
    let gc = dir.join(GC_DIR);
    let mut marks: std::collections::HashMap<String, (u64, Vec<PathBuf>)> = std::collections::HashMap::new();
    if let Ok(entries) = fs::read_dir(&gc) {
        for entry in entries.flatten().take(MAX_RECOVERED_FILES) {
            let name = entry.file_name();
            let Some((hash, at)) = name.to_str().and_then(|n| n.split_once('.')) else { continue };
            let (Some(at), true) = (at.parse::<u64>().ok(), pack::hash_ok(hash)) else { continue };
            let slot = marks.entry(hash.to_owned()).or_insert((at, Vec::new()));
            slot.0 = slot.0.min(at);
            slot.1.push(entry.path());
        }
    }
    let mut present = BTreeSet::new();
    if let Ok(entries) = fs::read_dir(dir.join(OBJECTS_DIR)) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(hash) = name.to_str().filter(|n| pack::hash_ok(n)) else { continue };
            present.insert(hash.to_owned());
            if needed.contains(hash) {
                continue;
            }
            match marks.get(hash) {
                None => {
                    let _ = private_dir(&gc).and_then(|()| private_file(&gc.join(format!("{hash}.{now}")), true).map(drop));
                }
                Some((since, files)) if now.saturating_sub(*since) >= ORPHAN_GRACE_SECS => {
                    if fs::remove_file(entry.path()).is_ok() {
                        for file in files {
                            let _ = fs::remove_file(file);
                        }
                    }
                }
                Some(_) => {}
            }
        }
    }
    // Marks of objects needed again, or gone.
    for (hash, (_, files)) in &marks {
        if needed.contains(hash) || !present.contains(hash) {
            for file in files {
                let _ = fs::remove_file(file);
            }
        }
    }
}

/// Is `name` one of the immutable files of a document folder (a version file)?
#[cfg(not(target_arch = "wasm32"))]
fn version_file_ok(name: &str) -> bool {
    [PARTS_SUFFIX, META_SUFFIX, ".bin"].iter().any(|suffix| name.strip_suffix(suffix).is_some_and(id_ok))
}

/// Bring a document's folders in step: each gets the version files and objects another one has
/// and it lacks. They are immutable, so copying never overwrites anything.
#[cfg(not(target_arch = "wasm32"))]
fn reconcile(dirs: &[PathBuf]) {
    if dirs.len() < 2 {
        return;
    }
    let list = |dir: &Path, sub: Option<&str>, ok: fn(&str) -> bool| -> BTreeSet<String> {
        let at = sub.map_or_else(|| dir.to_path_buf(), |s| dir.join(s));
        fs::read_dir(at)
            .map(|entries| {
                entries.flatten().filter_map(|e| e.file_name().to_str().map(str::to_owned)).filter(|n| ok(n)).take(MAX_RECOVERED_FILES).collect()
            })
            .unwrap_or_default()
    };
    for (sub, ok) in [(Some(OBJECTS_DIR), pack::hash_ok as fn(&str) -> bool), (None, version_file_ok as fn(&str) -> bool)] {
        let sets: Vec<BTreeSet<String>> = dirs.iter().map(|dir| list(dir, sub, ok)).collect();
        for (i, dir) in dirs.iter().enumerate() {
            for (j, other) in dirs.iter().enumerate() {
                if i == j {
                    continue;
                }
                let (Some(mine), Some(theirs)) = (sets.get(i), sets.get(j)) else { continue };
                for name in theirs.difference(mine) {
                    let (from, to) = match sub {
                        Some(s) => (other.join(s).join(name), dir.join(s).join(name)),
                        None => (other.join(name), dir.join(name)),
                    };
                    if fs::symlink_metadata(&to).is_err() {
                        let _ = copy_atomic(&from, &to);
                    }
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn store_save(roots: &[PathBuf], write: VersionWrite) -> Result<(Vec<VersionInfo>, Vec<PathBuf>), String> {
    let now = crate::cmd::now_unix();
    store_save_at(roots, write, now)
}

/// A document folder in each history folder, brought up to date: the layout of earlier builds
/// migrated, a document with an id given the history it had by path, and the folders
/// reconciled.
#[cfg(not(target_arch = "wasm32"))]
fn prepare_dirs(
    roots: &[PathBuf],
    path: &Path,
    document_id: Option<&str>,
    cloud: &[wordcraft_cloud::CloudRoot],
    now: u64,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (dirs, unavailable) = bucket_dirs(roots, path, document_id);
    for (dir, root) in dirs.iter().zip(roots.iter().filter(|r| !unavailable.contains(r))) {
        migrate_layout(dir);
        // The history this file had by path before it carried an id moves under the id.
        if document_id.is_some() {
            let by_path = document_dir(root, path);
            if by_path != *dir && fs::symlink_metadata(&by_path).is_ok() {
                let _ = merge_bucket(&by_path, dir, path, cloud, now);
            }
        }
    }
    reconcile(&dirs);
    (dirs, unavailable)
}

#[cfg(not(target_arch = "wasm32"))]
fn store_save_at(roots: &[PathBuf], mut write: VersionWrite, now: u64) -> Result<(Vec<VersionInfo>, Vec<PathBuf>), String> {
    let size = u64::try_from(write.bytes.len()).map_err(|_| "version is too large".to_string())?;
    if size > MAX_DOCUMENT_BYTES {
        return Err("version exceeds the 500 MiB document history limit".into());
    }
    let (dirs, unavailable) = prepare_dirs(roots, &write.path, write.document_id.as_deref(), &write.cloud, now);
    let dir = dirs.first().cloned().ok_or("no version history folder can be reached")?;
    private_dir(&dir).map_err(|e| e.to_string())?;
    let list = visible(&read_index(&dir));
    let digest = digest_hex(&write.bytes);
    let window_start = |v: &VersionInfo| v.window_start.clone().unwrap_or_else(|| v.saved_at.clone());
    // AutoSave joins the newest version only when its own editing session opened that window
    // less than AUTOSAVE_WINDOW_SECS ago. Joining writes a new version that supersedes it:
    // nothing already written is ever rewritten.
    let replace = write.kind == VersionKind::Autosave
        && write.session.is_some()
        && list.first().is_some_and(|v| {
            v.kind == VersionKind::Autosave
                && v.session == write.session
                && now.saturating_sub(parse_iso_secs(&window_start(v)).unwrap_or(0)) < AUTOSAVE_WINDOW_SECS
        });
    let replaced = if replace { list.first().cloned() } else { None };
    let id = new_id()?;
    let parent_info = match &replaced {
        Some(old) => old.details.as_ref().and_then(|d| d.parent.clone()).and_then(|p| list.iter().find(|v| v.id == p).cloned()),
        None => list.first().cloned(),
    };
    let details = &mut write.details;
    details.parent = parent_info.as_ref().map(|v| v.id.clone());
    if details.os_version.is_empty() {
        details.os_version = meta::os_version();
    }
    if let Some(before) = details.before.as_mut()
        && before.sha256.is_none()
    {
        before.sha256 = parent_info.as_ref().and_then(|v| v.digest.clone());
    }
    let captured_at = matches!(write.kind, VersionKind::Original | VersionKind::External).then(|| crate::cmd::iso_from_unix_secs(now));
    let session = if write.kind == VersionKind::Autosave { write.session.clone() } else { None };
    let mut info = VersionInfo {
        id: id.clone(),
        saved_at: write.saved_at.clone(),
        author: write.author.clone(),
        size,
        kind: write.kind,
        window_start: replaced.as_ref().map(window_start),
        digest: Some(digest),
        session,
        captured_at,
        storage: VersionStorage::Whole,
        stored: Some(size),
        supersedes: replaced.as_ref().map(|v| v.id.clone()),
        document_id: write.document_id.clone(),
        details: Some(Box::new(write.details.clone())),
        written: Some(write_order(now)),
    };
    // A zip package is kept as parts; the `original` and `external` copies are kept byte for
    // byte, as they were on disk. Anything that goes wrong with the parts falls back to the
    // whole bytes. An AutoSave that replaces the newest version deltas against the one before
    // it, so the replaced version's parts are not kept alive as a base.
    let verbatim = matches!(write.kind, VersionKind::Original | VersionKind::External);
    let base_list: &[VersionInfo] = if replace { list.get(1..).unwrap_or_default() } else { &list };
    let base = newest_manifest(&dir, base_list);
    let parent_manifest = parent_info.as_ref().and_then(|v| read_manifest(&dir, &v.id));
    let as_parts = !verbatim
        && write.bytes.starts_with(b"PK\x03\x04")
        && match write_parts_version(&dir, &info, &write.bytes, base.as_ref(), parent_manifest.as_ref()) {
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
        info.stored = Some(write_whole_version(&dir, &info, &write.bytes)?);
    }
    let all = read_index(&dir);
    migrate_one(&dir, &visible(&all));
    let (kept, removed) = prune(&dir, read_index(&dir), now);
    for (i, other) in dirs.iter().enumerate() {
        if i > 0 {
            remove_versions(other, &removed);
        }
        collect_objects(other, &kept, now);
    }
    reconcile(&dirs);
    if write.document_id.is_none() {
        // The identity record only helps find a moved file; failing to write it loses nothing.
        let _ = write_meta(&dir, &write.path, &write.cloud);
    }
    let mut kept = kept;
    // `stored` of the version just written is known here, not in its immutable record.
    if let Some(entry) = kept.iter_mut().find(|v| v.id == info.id) {
        entry.stored = info.stored;
    }
    Ok((kept, unavailable))
}

/// The digest of a stored version: from its record, or computed from its file for older ones.
#[cfg(not(target_arch = "wasm32"))]
fn entry_digest(dirs: &[PathBuf], entry: &VersionInfo) -> Option<String> {
    if let Some(digest) = &entry.digest {
        return Some(digest.clone());
    }
    let bytes = dirs.iter().find_map(|dir| read_small(&dir.join(format!("{}.bin", entry.id)), MAX_DOCUMENT_BYTES))?;
    Some(digest_hex(&bytes))
}

/// Move the history in `from_dir` into `to_dir` (the folder of `to_path`), merging with what
/// `to_dir` holds. Every file keeps its name (random ids, content hashes), so merging never
/// overwrites.
#[cfg(not(target_arch = "wasm32"))]
fn merge_bucket(from_dir: &Path, to_dir: &Path, to_path: &Path, cloud: &[wordcraft_cloud::CloudRoot], now: u64) -> Result<Vec<VersionInfo>, String> {
    let _ = now;
    if from_dir == to_dir {
        return Ok(visible(&read_index(to_dir)));
    }
    migrate_layout(from_dir);
    let moving = read_index(from_dir);
    if moving.is_empty() {
        return Ok(visible(&read_index(to_dir)));
    }
    let by_path = to_dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.len() == 16);
    if fs::symlink_metadata(to_dir).is_err() && fs::rename(from_dir, to_dir).is_ok() {
        if by_path {
            let _ = write_meta(to_dir, to_path, cloud);
        }
        return Ok(visible(&read_index(to_dir)));
    }
    private_dir(to_dir).map_err(|e| e.to_string())?;
    migrate_layout(to_dir);
    let mut left_behind = false;
    // Objects first, so a moved manifest never names a part that is not there yet.
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
            if fs::rename(&source, &target).is_err() && copy_atomic(&source, &target).is_err() {
                left_behind = true;
            }
        }
    }
    if let Ok(entries) = fs::read_dir(from_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_str().is_some_and(version_file_ok) {
                continue;
            }
            let target = to_dir.join(&name);
            if fs::symlink_metadata(&target).is_ok() {
                continue;
            }
            let source = entry.path();
            if fs::rename(&source, &target).is_err() && copy_atomic(&source, &target).is_err() {
                left_behind = true;
            }
        }
    }
    if by_path {
        let _ = write_meta(to_dir, to_path, cloud);
    }
    if !left_behind {
        let _ = fs::remove_dir_all(from_dir);
    }
    Ok(visible(&read_index(to_dir)))
}

/// The history of a file kept by path that was moved or renamed outside WordCraft. A folder
/// qualifies when the file it last belonged to no longer exists (a copy keeps its original, so
/// a copy never takes the history, as with OneDrive, where only a move keeps it) and its newest
/// version holds exactly `digest`, the bytes now at `path`. Several candidates: the most
/// recently saved wins.
#[cfg(not(target_arch = "wasm32"))]
fn adopt_moved(root: &Path, path: &Path, digest: &str, cloud: &[wordcraft_cloud::CloudRoot], now: u64) -> Option<Vec<VersionInfo>> {
    let target = document_dir(root, path);
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in fs::read_dir(root).ok()?.flatten().take(MAX_BUCKET_SCAN) {
        let dir = entry.path();
        if dir == target || !entry.file_name().to_str().is_some_and(|n| n.len() == 16 && key_ok(n)) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Some(record) = read_meta(&dir) else { continue };
        let Some(previous) = meta::resolve_portable_path(&record.path, cloud) else { continue };
        if same_file_path(&previous, path) || fs::symlink_metadata(&previous).is_ok() {
            continue;
        }
        let list = visible(&read_index(&dir));
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
    merge_bucket(&dir, &target, path, cloud, now).ok().filter(|list| !list.is_empty())
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

/// Make sure the bytes at `path` are in the document's history before a save replaces them,
/// storing them as an `original` version (no history yet) or an `external` one (another program
/// changed the file) when they are not. Returns what was on disk.
#[cfg(not(target_arch = "wasm32"))]
fn store_protect(
    roots: &[PathBuf],
    path: &Path,
    document_id: Option<&str>,
    mut details: VersionDetails,
    cloud: &[wordcraft_cloud::CloudRoot],
    now: u64,
) -> Result<Option<DiskBefore>, String> {
    let Some((bytes, file_meta)) = read_document(path)? else { return Ok(None) };
    let digest = digest_hex(&bytes);
    let modified = modified_secs(&file_meta);
    let before = DiskBefore { size: bytes.len() as u64, modified: modified.map(crate::cmd::iso_from_unix_secs), sha256: Some(digest.clone()) };
    if bytes.is_empty() {
        // An empty file (File Explorer's New › Word document) has nothing to go back to.
        return Ok(Some(before));
    }
    let (dirs, _) = prepare_dirs(roots, path, document_id, cloud, now);
    let Some(dir) = dirs.first() else { return Err("no version history folder can be reached".into()) };
    let mut list = visible(&read_union(&dirs));
    if list.is_empty()
        && document_id.is_none()
        && let Some(root) = roots.iter().find(|r| dir.starts_with(r))
        && let Some(adopted) = adopt_moved(root, path, &digest, cloud, now)
    {
        list = adopted;
    }
    if list.iter().any(|entry| entry_digest(&dirs, entry).as_deref() == Some(digest.as_str())) {
        return Ok(Some(before));
    }
    let kind = if list.is_empty() { VersionKind::Original } else { VersionKind::External };
    let saved_at = modified.map_or(now, |at| at.min(now));
    details.trigger = "beforeSave".into();
    details.before = None;
    details.stats = None;
    details.properties = None;
    store_save_at(
        roots,
        VersionWrite {
            path: path.to_path_buf(),
            bytes,
            author: String::new(),
            kind,
            saved_at: crate::cmd::iso_from_unix_secs(saved_at),
            session: None,
            document_id: document_id.map(str::to_owned),
            details,
            cloud: cloud.to_vec(),
        },
        now,
    )
    .map(|_| Some(before))
}

#[cfg(not(target_arch = "wasm32"))]
fn store_list(
    roots: &[PathBuf],
    path: &Path,
    document_id: Option<&str>,
    probe: bool,
    cloud: &[wordcraft_cloud::CloudRoot],
) -> Result<Vec<VersionInfo>, String> {
    let (dirs, _) = bucket_dirs(roots, path, document_id);
    let mut list = visible(&read_union(&dirs));
    if list.is_empty() && document_id.is_some() {
        // A file that only now carries an id: its history by path, until the next save moves it.
        let by_path: Vec<PathBuf> = roots.iter().map(|root| document_dir(root, path)).collect();
        list = visible(&read_union(&by_path));
    }
    if !list.is_empty() || !probe || document_id.is_some() {
        return Ok(list);
    }
    let Ok(Some((bytes, _))) = read_document(path) else { return Ok(list) };
    if bytes.is_empty() {
        return Ok(list);
    }
    let digest = digest_hex(&bytes);
    let now = crate::cmd::now_unix();
    Ok(roots.iter().find_map(|root| adopt_moved(root, path, &digest, cloud, now)).unwrap_or(list))
}

#[cfg(not(target_arch = "wasm32"))]
fn store_rename(roots: &[PathBuf], from: &Path, to: &Path) -> Result<Vec<VersionInfo>, String> {
    let now = crate::cmd::now_unix();
    let mut list = Vec::new();
    for root in roots {
        if fs::symlink_metadata(root).is_err() {
            continue;
        }
        list = merge_bucket(&document_dir(root, from), &document_dir(root, to), to, &[], now)?;
    }
    Ok(list)
}

/// Copy every document folder of the history folders `from` that are not in `to` into each of
/// `to`, and with `keep_old` false delete them from `from` once copied.
#[cfg(not(target_arch = "wasm32"))]
fn store_relocate(from: &[PathBuf], to: &[PathBuf], keep_old: bool) -> Result<(), String> {
    let mut failed = false;
    for old in from.iter().filter(|old| !to.contains(old)) {
        let Ok(entries) = fs::read_dir(old) else { continue };
        for entry in entries.flatten().take(MAX_BUCKET_SCAN) {
            let name = entry.file_name();
            if !name.to_str().is_some_and(key_ok) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            migrate_layout(&entry.path());
            let mut dirs = vec![entry.path()];
            for new in to {
                if private_dir(new).is_ok() {
                    dirs.push(new.join(&name));
                    let _ = private_dir(&new.join(&name));
                } else {
                    failed = true;
                }
            }
            reconcile(&dirs);
            // Every new folder holds every file before the old one goes.
            let complete = dirs.iter().skip(1).all(|dir| {
                read_index(dir).len() >= read_index(&entry.path()).len()
                    && fs::read_dir(entry.path().join(OBJECTS_DIR))
                        .map(|objects| objects.flatten().all(|o| dir.join(OBJECTS_DIR).join(o.file_name()).is_file()))
                        .unwrap_or(true)
            });
            if !keep_old && complete && dirs.len() > 1 {
                let _ = fs::remove_dir_all(entry.path());
            } else if !complete {
                failed = true;
            }
        }
    }
    if failed { Err("some version history could not be copied to the new folder".into()) } else { Ok(()) }
}

/// Delete a document's history (or all history) in every history folder. Returns how many
/// document folders went.
#[cfg(not(target_arch = "wasm32"))]
fn store_delete(roots: &[PathBuf], scope: DeleteScope, path: Option<&Path>, document_id: Option<&str>) -> Result<usize, String> {
    let mut deleted = 0;
    let mut failed = None;
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else { continue };
        let targets: Vec<PathBuf> = match scope {
            DeleteScope::All => entries
                .flatten()
                .filter(|e| e.file_name().to_str().is_some_and(key_ok) && e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect(),
            DeleteScope::Document => {
                let mut t = Vec::new();
                if let Some(id) = document_id {
                    t.push(root.join(meta::id_bucket(id)));
                }
                if let Some(path) = path {
                    t.push(document_dir(root, path));
                    t.push(root.join(legacy_key(path)));
                }
                t
            }
        };
        for dir in targets {
            if fs::symlink_metadata(&dir).is_err() {
                continue;
            }
            match fs::remove_dir_all(&dir) {
                Ok(()) => deleted += 1,
                Err(e) => failed = Some(e.to_string()),
            }
        }
    }
    match failed {
        Some(error) => Err(format!("some version history could not be deleted: {error}")),
        None => Ok(deleted),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn hold(lock: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Write `bytes` to a private file (used for the installation id).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_atomic(path, bytes)
}

#[cfg(not(target_arch = "wasm32"))]
impl Versions {
    pub fn start(root: PathBuf, roots: Vec<PathBuf>, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        private_dir(&root).map_err(|e| e.to_string())?;
        let installation = meta::installation_id(root.parent().unwrap_or(&root));
        let roots = if roots.is_empty() { vec![root.clone()] } else { roots };
        let (jobs, rx_jobs) = mpsc::sync_channel(16);
        let (events, rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared { roots, unavailable: Vec::new(), relocating: false }));
        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("wordcraft-versions".into())
            .spawn(move || {
                while let Ok(job) = rx_jobs.recv() {
                    let mut guard = hold(&worker_shared);
                    let roots = guard.roots.clone();
                    let (path, result) = match job {
                        Job::Save(write) => {
                            let path = write.path.clone();
                            let result = store_save(&roots, *write).map(|(list, unavailable)| {
                                guard.unavailable = unavailable;
                                list
                            });
                            (path, result)
                        }
                        Job::List { path, document_id, probe, cloud } => {
                            let result = store_list(&roots, &path, document_id.as_deref(), probe, &cloud);
                            (path, result)
                        }
                        Job::Rename { from, to } => {
                            let result = store_rename(&roots, &from, &to);
                            (to, result)
                        }
                        Job::Relocate { from, to, keep_old } => {
                            let result = store_relocate(&from, &to, keep_old).map(|()| Vec::new());
                            guard.relocating = false;
                            (PathBuf::new(), result)
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
            default_root: root,
            shared,
            run_id: new_id()?,
            installation,
            verified: None,
            identity_checked: None,
            bag: None,
            list: Vec::new(),
            ready: false,
            busy: false,
            error: None,
            current: None,
            alive: true,
        })
    }

    fn roots(&self) -> Vec<PathBuf> {
        hold(&self.shared).roots.clone()
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
                    match event.result {
                        Ok(()) => {
                            if self.current.as_ref().is_some_and(|(path, _)| *path == event.path) {
                                self.list = event.list;
                            }
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

    /// Show the history of a document. The versions' records are small and cached, so they are
    /// read here at once and the first `file.versions` after opening a document already lists
    /// them. Only when there are none does the worker look for the history of a moved file.
    pub fn ensure_path(&mut self, path: &Path, document_id: Option<&str>, cloud: &[wordcraft_cloud::CloudRoot]) {
        let canonical = canonical_path(path);
        let key = (canonical.clone(), document_id.map(str::to_owned));
        if self.current.as_ref() == Some(&key) {
            return;
        }
        let roots = self.roots();
        self.list = store_list(&roots, &canonical, document_id, false, cloud).unwrap_or_default();
        self.ready = true;
        self.current = Some(key);
        if !self.list.is_empty() || document_id.is_some() {
            return;
        }
        // The worker may read the document to find a moved file's history; only where this
        // thread may read it (automation roots).
        let probe = crate::file_access::check_read(&canonical.to_string_lossy()).is_ok();
        if probe && let Err(error) = self.queue(Job::List { path: canonical, document_id: None, probe, cloud: cloud.to_vec() }) {
            self.error = Some(error);
        }
    }

    fn save(&mut self, write: VersionWrite) {
        if write.bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            self.error = Some("version exceeds the 500 MiB document history limit".into());
            return;
        }
        if let Err(error) = self.queue(Job::Save(Box::new(write))) {
            self.error = Some(error);
        }
    }

    /// The history of a document kept by path follows it when `file.rename` moves it, in order
    /// with the saves queued before it. (A document with an id needs nothing: the id moves with
    /// the file.)
    pub fn renamed(&mut self, from: PathBuf, to: PathBuf) {
        let to = canonical_path(&to);
        if let Err(error) = self.queue(Job::Rename { from, to }) {
            // The folder stays under the old name; the moved-file search still finds it at the
            // next save, since the old path no longer exists.
            self.error = Some(error);
        }
    }

    /// The history folders in use, and those the last write could not reach.
    pub(crate) fn folders(&self) -> (Vec<PathBuf>, Vec<PathBuf>, bool) {
        let shared = hold(&self.shared);
        (shared.roots.clone(), shared.unavailable.clone(), shared.relocating)
    }

    /// Use `roots` from now on; what the old folders hold is copied, moved or left.
    pub(crate) fn set_folders(&mut self, roots: Vec<PathBuf>, existing: Relocation) -> Result<(), String> {
        let roots = if roots.is_empty() { vec![self.default_root.clone()] } else { roots };
        let old = {
            let mut shared = hold(&self.shared);
            let old = std::mem::replace(&mut shared.roots, roots.clone());
            shared.unavailable.clear();
            shared.relocating = existing != Relocation::Leave && old != roots;
            old
        };
        self.current = None;
        if existing != Relocation::Leave && old != roots {
            self.queue(Job::Relocate { from: old, to: roots, keep_old: existing == Relocation::Copy })?;
        }
        Ok(())
    }

    /// Synchronously keep the bytes at `path` before they are overwritten (see [`store_protect`]).
    fn protect(
        &mut self,
        document_id: u64,
        path: &Path,
        doc_uuid: Option<&str>,
        details: VersionDetails,
        cloud: &[wordcraft_cloud::CloudRoot],
    ) -> Result<Option<DiskBefore>, String> {
        let canonical = canonical_path(path);
        if self.verified.as_ref().is_some_and(|(id, verified)| *id == document_id && *verified == canonical) {
            return Ok(None);
        }
        let result = {
            let mut shared = hold(&self.shared);
            let roots = shared.roots.clone();
            let result = store_protect(&roots, &canonical, doc_uuid, details, cloud, crate::cmd::now_unix());
            let (_, unavailable) = bucket_dirs(&roots, &canonical, doc_uuid);
            shared.unavailable = unavailable;
            result
        };
        if result.is_ok() {
            self.verified = Some((document_id, canonical));
            self.current = None;
        }
        result
    }

    pub fn read(&self, path: &Path, document_id: Option<&str>, id: &str) -> Result<(VersionInfo, Vec<u8>), String> {
        if !id_ok(id) {
            return Err("invalid version id".into());
        }
        // Held while parts are read, so the worker's collection of unused parts cannot remove
        // one in between.
        let shared = hold(&self.shared);
        let mut dirs = bucket_dirs(&shared.roots, path, document_id).0;
        if document_id.is_some() {
            dirs.extend(shared.roots.iter().map(|root| document_dir(root, path)));
        }
        let info = read_union(&dirs).into_iter().find(|entry| entry.id == id).ok_or_else(|| "no such version".to_string())?;
        let bytes = match info.storage {
            VersionStorage::Parts => read_parts_version(&dirs, id)?,
            VersionStorage::Whole => read_whole_version(&dirs, &info)?,
        };
        Ok((info, bytes))
    }

    /// Delete history now (and in every mirror folder).
    fn delete(&mut self, scope: DeleteScope, path: Option<&Path>, document_id: Option<&str>) -> Result<usize, String> {
        let shared = hold(&self.shared);
        let result = store_delete(&shared.roots, scope, path.map(canonical_path).as_deref(), document_id);
        drop(shared);
        self.list.clear();
        self.current = None;
        self.verified = None;
        result
    }

    /// The newest version of a document and the path it was saved at, for telling a copy.
    fn newest(&self, path: &Path, document_id: &str, cloud: &[wordcraft_cloud::CloudRoot]) -> Option<(String, Option<PathBuf>)> {
        let roots = self.roots();
        let (dirs, _) = bucket_dirs(&roots, path, Some(document_id));
        let newest = visible(&read_union(&dirs)).into_iter().next()?;
        let saved_at = newest.details.as_ref().and_then(|d| meta::resolve_portable_path(&d.path, cloud));
        Some((newest.id, saved_at))
    }
}

impl crate::Session {
    /// Desktop host wiring for the local history folder: `root` is the settings folder's
    /// `versions`, the default; the folders in File › Options › Save apply on top.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn enable_versions(&mut self, root: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Result<(), String> {
        let roots = version_roots(&root, &self.prefs);
        self.version_store = Some(Versions::start(root, roots, wake)?);
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

    /// The document whose history is shown: the one a previous version was opened from.
    fn history_document(&self) -> &wordcraft_doc::Document {
        self.opened_version.as_ref().map_or(&self.doc, |version| &version.original_doc)
    }

    /// The id the history of the open document is kept under: a Word package's id (none for
    /// other formats, kept by path, or with personal information removed).
    pub fn version_document_id(&self) -> Option<String> {
        let doc = self.history_document();
        let path = self.opened_version.as_ref().map(|version| version.path.clone()).or_else(|| self.path.clone());
        history_id(doc, path.as_deref())
    }

    pub fn version_infos(&mut self) -> Vec<VersionInfo> {
        self.versions_tick();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let id = self.version_document_id();
            let cloud = self.cloud.roots.clone();
            if let (Some(path), Some(store)) =
                (self.opened_version.as_ref().map(|version| version.path.clone()).or_else(|| self.path.clone()), self.version_store.as_mut())
            {
                store.ensure_path(&path, id.as_deref(), &cloud);
                return store.list.clone();
            }
        }
        Vec::new()
    }

    /// The editing session AutoSave versions are grouped by: this run of the app and this
    /// opening of the document.
    #[cfg(not(target_arch = "wasm32"))]
    fn version_session(&self) -> Option<String> {
        self.version_store.as_ref().map(|store| format!("{}-{}", store.run_id, self.document_id()))
    }

    /// What a version records about this save (see [`VersionDetails`]), privacy first: no
    /// author or initials when the document removes personal information, a random
    /// installation id and the user's device label instead of the computer's name, the path
    /// without the account, and counts instead of text.
    #[cfg(not(target_arch = "wasm32"))]
    fn version_details(&mut self, path: &Path, trigger: &str, before: Option<DiskBefore>, with_stats: bool) -> VersionDetails {
        let now = crate::cmd::now_unix();
        let private = self.doc.settings.remove_personal_info;
        let opened = self.document_opened_at();
        let mut details = VersionDetails {
            utc_offset: local_offset(now),
            session_start: (opened > 0).then(|| crate::cmd::iso_from_unix_secs(opened)),
            editing_seconds: (opened > 0).then(|| now.saturating_sub(opened)),
            trigger: trigger.to_owned(),
            initials: if private { String::new() } else { self.user_initials() },
            installation: self.version_store.as_ref().map(|s| s.installation.clone()).unwrap_or_default(),
            device: self.prefs.device_label.clone().filter(|l| !l.trim().is_empty()).unwrap_or_else(|| default_device_label().to_owned()),
            os: std::env::consts::OS.to_owned(),
            os_version: String::new(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            commit: option_env!("WORDCRAFT_COMMIT").map(str::to_owned),
            path: meta::portable_path(path, &self.cloud.roots),
            provider: meta::provider_of(path, &self.cloud.roots).to_owned(),
            before,
            ..VersionDetails::default()
        };
        if with_stats {
            let core = &self.doc.core;
            details.properties = Some(VersionProperties {
                title: core.title.clone(),
                creator: if private { String::new() } else { core.creator.clone() },
                last_modified_by: if private { String::new() } else { core.last_modified_by.clone() },
                revision: u64::from(core.revision),
            });
            let text = self.doc.plain_text(wordcraft_doc::StoryRef::Body);
            let bag = meta::word_bag(&text);
            let document = self.document_id();
            let diff =
                self.version_store.as_ref().and_then(|s| s.bag.as_ref()).filter(|(id, _)| *id == document).map(|(_, old)| meta::bag_diff(old, &bag));
            if let Some(store) = self.version_store.as_mut() {
                store.bag = Some((document, bag));
            }
            details.stats = Some(VersionStats {
                words: self.doc.word_count() as u64,
                characters: text.chars().filter(|c| !matches!(c, '\n' | '\r')).count() as u64,
                paragraphs: self.doc.paragraph_count() as u64,
                pages: self.cached_page_count().map(|n| n as u64),
                comments: self.doc.comments.len() as u64,
                tracked_changes: self.doc.revisions.len() as u64,
                words_inserted: diff.map(|d| d.0),
                words_removed: diff.map(|d| d.1),
            });
        }
        details
    }

    /// The words of a document just opened: the next version counts its edit against them.
    pub(crate) fn versions_document_opened(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.version_store.is_some() {
            let bag = meta::word_bag(&self.doc.plain_text(wordcraft_doc::StoryRef::Body));
            let document = self.document_id();
            if let Some(store) = self.version_store.as_mut() {
                store.bag = Some((document, bag));
                store.identity_checked = None;
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn record_version_with(
        &mut self,
        path: &Path,
        bytes: Vec<u8>,
        kind: VersionKind,
        trigger: &str,
        document_id: Option<String>,
        origin: Option<VersionOrigin>,
        before: Option<DiskBefore>,
    ) {
        if self.version_store.is_none() {
            return;
        }
        let session = if kind == VersionKind::Autosave && trigger != "beforeRestore" { self.version_session() } else { None };
        let mut details = self.version_details(path, trigger, before, true);
        details.origin = origin;
        let author = if self.doc.settings.remove_personal_info { String::new() } else { self.author.clone() };
        let write = VersionWrite {
            path: canonical_path(path),
            bytes,
            author,
            kind,
            saved_at: crate::cmd::now_iso(),
            session,
            document_id,
            details,
            cloud: self.cloud.roots.clone(),
        };
        if let Some(store) = self.version_store.as_mut() {
            store.save(write);
        }
    }

    /// Before `file.save` writes `path` (`ext` its format): settle the document's identity and
    /// keep what is on disk.
    ///
    /// - A Word package gets a random document id, in its custom properties, at its first save
    ///   (made here or received from someone else). Save As (and Save to Cloud, Keep both) makes
    ///   a new file: it gets a new id and records which document and version it came from. A
    ///   file that carries the id of another file that still exists (a copy made outside the
    ///   app, or two copies open at once) gets a new id at its next save, also recording where
    ///   it came from. Other formats have no id and keep their history by path.
    /// - With "remove personal information" on, no id is written into the file (the history
    ///   is kept by path) and versions record no author.
    /// - Then the bytes on disk are kept if the history lacks them (an `original` or an
    ///   `external` version), synchronously, before the file is replaced.
    ///
    /// Nothing here can fail the save.
    pub(crate) fn prepare_version(&mut self, path: &Path, ext: &str, trigger: Option<&str>, autosave: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.version_store.is_none() {
                return;
            }
            let word = crate::io::is_word_package(ext);
            let own = self.path.as_deref().is_some_and(|own| same_file_path(own, path));
            let private = self.doc.settings.remove_personal_info;
            let mut kind = self.version_next_kind.take();
            let mut trigger = trigger.map(str::to_owned).unwrap_or_else(|| if autosave { "autosave".into() } else { "save".into() });
            let mut origin = None;
            let current = document_uuid(&self.doc);
            let cloud = self.cloud.roots.clone();
            let document = self.document_id();
            let canonical = canonical_path(path);
            let mut id = None;
            if word && private {
                self.doc.remove_custom_prop(meta::DOC_ID_PROP);
            } else if word {
                let previous_history = current.as_deref().and_then(|cur| self.version_store.as_ref().and_then(|s| s.newest(path, cur, &cloud)));
                if !own && self.path.is_some() {
                    // A new file from this document.
                    origin = Some(VersionOrigin { document_id: current.clone(), version_id: previous_history.map(|(v, _)| v) });
                    kind = kind.or(Some(VersionKind::SaveAs));
                    trigger = if kind == Some(VersionKind::ConflictCopy) { "conflictCopy".into() } else { "saveAs".into() };
                } else if let Some(cur) = current.clone() {
                    let checked = self.version_store.as_ref().is_some_and(|s| s.identity_checked.as_ref() == Some(&(document, canonical.clone())));
                    if !checked {
                        if let Some((version, Some(saved_at))) = previous_history
                            && !same_file_path(&saved_at, path)
                            && saved_at.is_file()
                            && meta::file_document_id(&saved_at).as_deref() == Some(cur.as_str())
                        {
                            // Another file with this id is still there: this one is a copy.
                            origin = Some(VersionOrigin { document_id: Some(cur.clone()), version_id: Some(version) });
                            trigger = "copy".into();
                        } else {
                            id = Some(cur);
                        }
                    } else {
                        id = Some(cur);
                    }
                }
                if id.is_none() {
                    match meta::new_document_id() {
                        Ok(new) => id = Some(new),
                        Err(error) => {
                            if let Some(store) = self.version_store.as_mut() {
                                store.error = Some(error);
                            }
                        }
                    }
                }
                if let Some(new) = &id {
                    if current.as_deref() != Some(new.as_str()) {
                        self.doc.set_custom_prop(meta::DOC_ID_PROP, new);
                    }
                    if let Some(store) = self.version_store.as_mut() {
                        store.identity_checked = Some((document, canonical.clone()));
                    }
                }
            }
            // Keep what is on disk, into the history this save goes to.
            let changed_outside =
                own && self.cloud.stamp.as_mut().is_some_and(|stamp| !matches!(stamp.check(path), Ok(wordcraft_cloud::DiskState::Unchanged)));
            let details = self.version_details(path, "beforeSave", None, false);
            let mut before = None;
            if let Some(store) = self.version_store.as_mut() {
                if changed_outside || !own {
                    store.verified = None;
                }
                if crate::file_access::check_read(&path.to_string_lossy()).is_ok() {
                    match store.protect(document, path, id.as_deref(), details, &cloud) {
                        Ok(found) => before = found,
                        Err(error) => {
                            log::warn!("version history could not keep {} before saving: {error}", path.display());
                            store.error = Some(error);
                        }
                    }
                }
            }
            if before.is_none()
                && let Ok(meta) = fs::metadata(path)
            {
                // Known to hold the previous version; its hash comes from that version's record.
                before = Some(DiskBefore { size: meta.len(), modified: modified_secs(&meta).map(crate::cmd::iso_from_unix_secs), sha256: None });
            }
            self.version_pending = Some(PendingVersion { kind, origin, trigger, before, document_id: id, path: canonical });
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (path, ext, trigger, autosave);
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
    /// or move. A document with an id needs nothing moved.
    pub(crate) fn versions_renamed(&mut self, from: PathBuf, to: &Path) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let document_id = self.document_id();
            let by_path = self.version_document_id().is_none();
            if let Some(store) = self.version_store.as_mut() {
                let verified = store.verified.as_ref().is_some_and(|(id, path)| *id == document_id && *path == from);
                if by_path {
                    store.renamed(from, to.to_path_buf());
                }
                store.verified = verified.then(|| (document_id, canonical_path(to)));
                store.identity_checked = store.identity_checked.take().filter(|(id, _)| *id == document_id).map(|(id, _)| (id, canonical_path(to)));
                store.current = None;
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (from, to);
    }

    pub(crate) fn record_saved_command(&mut self, id: &str, params: &serde_json::Value, result: &serde_json::Value) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let pending = self.version_pending.take();
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
            if !self.path.as_deref().is_some_and(|own| same_file_path(own, &path)) {
                return;
            }
            let autosave = id == "file.save" && params.get("ifUnchanged").and_then(serde_json::Value::as_bool) == Some(true);
            let pending = pending.filter(|p| same_file_path(&p.path, &path)).unwrap_or_default();
            let kind = pending.kind.unwrap_or(if autosave { VersionKind::Autosave } else { VersionKind::Save });
            let trigger = if pending.trigger.is_empty() { if autosave { "autosave".to_owned() } else { "save".to_owned() } } else { pending.trigger };
            let document_id = if pending.path.as_os_str().is_empty() { history_id(&self.doc, Some(&path)) } else { pending.document_id };
            self.mark_verified(&path);
            self.record_version_with(&path, bytes, kind, &trigger, document_id, pending.origin, pending.before);
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
                    let id = history_id(current, Some(source));
                    self.record_version_with(source, bytes, VersionKind::Autosave, "beforeRestore", id, None, None);
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

    /// Restore replaced the document with version `version`: record the restore at once with
    /// those exact bytes, as OneDrive makes the restored version the newest one in the list
    /// right away (whether or not AutoSave writes the file afterwards).
    pub(crate) fn record_restore(&mut self, source: &Path, bytes: Vec<u8>, version: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let id = self.version_document_id();
            self.record_version_with(source, bytes, VersionKind::Restore, &format!("restore:{version}"), id, None, None);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (source, bytes, version);
    }

    pub(crate) fn read_version(&mut self, path: &Path, id: &str) -> Result<(VersionInfo, Vec<u8>), String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.versions_tick();
            let document = self.version_document_id();
            let store = self.version_store.as_ref().ok_or_else(|| "local version history is unavailable".to_string())?;
            store.read(path, document.as_deref(), id)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (path, id);
            Err("local version history is available on desktop only".into())
        }
    }

    /// Delete the open document's history, or all history, in every history folder.
    pub(crate) fn delete_version_history(&mut self, scope: DeleteScope) -> Result<usize, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let document = self.version_document_id();
            let path = self.opened_version.as_ref().map(|version| version.path.clone()).or_else(|| self.path.clone());
            let store = self.version_store.as_mut().ok_or_else(|| "local version history is unavailable".to_string())?;
            store.delete(scope, path.as_deref(), document.as_deref())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = scope;
            Err("local version history is available on desktop only".into())
        }
    }

    /// File › Options › Save › Version history changed: use the folders it names now.
    pub(crate) fn apply_version_folders(&mut self, existing: Relocation) -> Result<(), String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let prefs = self.prefs.clone();
            if let Some(store) = self.version_store.as_mut() {
                let roots = version_roots(&store.default_root, &prefs);
                store.set_folders(roots, existing)?;
            }
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = existing;
            Ok(())
        }
    }

    /// The history folders in use (empty where there is no history), those the last write
    /// could not reach, and whether history is being copied to new folders.
    pub fn version_folders(&self) -> (Vec<PathBuf>, Vec<PathBuf>, bool) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = self.version_store.as_ref() {
            return store.folders();
        }
        (Vec::new(), Vec::new(), false)
    }

    pub fn previous_version(&self) -> Option<&VersionInfo> {
        self.opened_version.as_ref().map(|version| &version.info)
    }
}

/// The id `doc`'s history is kept under when saved at `path`: only Word packages carry one,
/// and not when the document removes personal information.
fn history_id(doc: &wordcraft_doc::Document, path: Option<&Path>) -> Option<String> {
    let word = path.and_then(|p| p.extension()).and_then(|e| e.to_str()).is_some_and(|e| crate::io::is_word_package(&e.to_ascii_lowercase()));
    if !word || doc.settings.remove_personal_info {
        return None;
    }
    document_uuid(doc)
}

/// The history folders the settings name: the settings folder's alone (the default), another
/// folder instead, or both.
#[cfg(not(target_arch = "wasm32"))]
fn version_roots(default: &Path, prefs: &crate::Prefs) -> Vec<PathBuf> {
    match prefs.versions_folder.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
        None => vec![default.to_path_buf()],
        Some(folder) if prefs.versions_mirror => vec![default.to_path_buf(), PathBuf::from(folder)],
        Some(folder) => vec![PathBuf::from(folder)],
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
    fn put(root: &Path, file: &Path, bytes: &[u8], kind: VersionKind, when: u64, now: u64, session: Option<&str>) -> Vec<VersionInfo> {
        store_save_at(
            &[root.to_path_buf()],
            VersionWrite {
                path: file.to_path_buf(),
                bytes: bytes.to_vec(),
                author: "Tester".into(),
                kind,
                saved_at: crate::cmd::iso_from_unix_secs(when),
                session: session.map(str::to_owned),
                document_id: None,
                details: VersionDetails { trigger: "save".into(), ..VersionDetails::default() },
                cloud: Vec::new(),
            },
            now,
        )
        .unwrap()
        .0
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn write(root: &Path, file: &Path, text: &str, kind: VersionKind, when: u64, now: u64) -> Vec<VersionInfo> {
        put(root, file, text.as_bytes(), kind, when, now, Some("run-1"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_bytes_at(root: &Path, file: &Path, bytes: &[u8], when: u64) -> Vec<VersionInfo> {
        put(root, file, bytes, VersionKind::Save, when, when, None)
    }

    /// A session with the history enabled in `root/versions`, never the real profile.
    #[cfg(not(target_arch = "wasm32"))]
    fn session_in(root: &Path, text: &str) -> crate::Session {
        let mut session = crate::Session::new(wordcraft_doc::Document::from_text(text));
        session.author = "Tester".into();
        session.initials = "TT".into();
        session.enable_versions(root.join("versions"), std::sync::Arc::new(|| {})).unwrap();
        session
    }

    /// The versions the session's store holds for `file` (under the session document's id, if
    /// any) once the worker has written at least `count`.
    #[cfg(not(target_arch = "wasm32"))]
    fn wait_versions(session: &crate::Session, file: &Path, count: usize) -> Vec<VersionInfo> {
        let store = session.version_store.as_ref().unwrap();
        let id = session.version_document_id();
        let mut list = Vec::new();
        for _ in 0..500 {
            list = {
                let shared = hold(&store.shared);
                store_list(&shared.roots, file, id.as_deref(), false, &[]).unwrap()
            };
            if list.len() >= count {
                return list;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        list
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn wait_idle(session: &mut crate::Session) {
        for _ in 0..500 {
            session.versions_tick();
            let store = session.version_store.as_ref().unwrap();
            if !store.busy && !hold(&store.shared).relocating {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn version_bytes(session: &crate::Session, file: &Path, id: &str) -> Vec<u8> {
        session.version_store.as_ref().unwrap().read(file, session.version_document_id().as_deref(), id).unwrap().1
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

    /// The same document text, whatever the zip around it.
    #[cfg(not(target_arch = "wasm32"))]
    fn same_text(a: &[u8], b: &[u8]) -> bool {
        let text = |bytes: &[u8]| crate::io::open_bytes("x.docx", bytes).map(|d| d.plain_text(wordcraft_doc::StoryRef::Body)).ok();
        text(a).is_some() && text(a) == text(b)
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

    /// A body of varied prose (it compresses like real text, not like a repeated line).
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
    fn read_at(root: &Path, file: &Path, id: &str) -> Result<Vec<u8>, String> {
        let dir = document_dir(root, file);
        let dirs = [dir.clone()];
        match read_index(&dir).into_iter().find(|v| v.id == id) {
            Some(info) if info.storage == VersionStorage::Parts => read_parts_version(&dirs, id),
            Some(info) => read_whole_version(&dirs, &info),
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

    /// Every version record (manifest or metadata file) under `dir`, as text.
    #[cfg(not(target_arch = "wasm32"))]
    fn records(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.extend(records(&path));
                } else if path.to_string_lossy().ends_with(".json") {
                    out.push(fs::read_to_string(&path).unwrap_or_default());
                }
            }
        }
        out
    }

    // ---- cadence and retention --------------------------------------------------------------

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn autosaves_join_within_ten_minutes_but_not_after() {
        let root = temp("join");
        let file = root.join("note.txt");
        let first = write(&root, &file, "one", VersionKind::Autosave, 1_800_000_000, 1_800_000_000);
        let second = write(&root, &file, "two", VersionKind::Autosave, 1_800_000_599, 1_800_000_599);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].size, 3);
        assert_eq!(second[0].supersedes.as_deref(), Some(first[0].id.as_str()), "a join writes a new record, never rewrites one");
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
        // Superseded records are gone from the folder too.
        let dir = document_dir(&root, &file);
        assert_eq!(read_index(&dir).len(), 4);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn first_autosave_after_reopening_starts_a_new_version() {
        let root = temp("reopen");
        let file = root.join("note.txt");
        let now = 1_800_000_000;
        assert_eq!(put(&root, &file, b"first run", VersionKind::Autosave, now, now, Some("run-1")).len(), 1);
        let list = put(&root, &file, b"second run", VersionKind::Autosave, now + 120, now + 120, Some("run-2"));
        assert_eq!(list.len(), 2, "a new editing session never overwrites the previous session's version");
        assert_eq!(list[1].size, "first run".len() as u64);
        assert_eq!(put(&root, &file, b"second run, more", VersionKind::Autosave, now + 180, now + 180, Some("run-2")).len(), 2);
        assert_eq!(put(&root, &file, b"kept", VersionKind::Autosave, now + 200, now + 200, None).len(), 3);
        assert_eq!(put(&root, &file, b"after", VersionKind::Autosave, now + 210, now + 210, Some("run-2")).len(), 4);
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
        let now: u64 = 1_800_000_000 / day * day;
        let mut entries = Vec::new();
        let mut n = 0usize;
        let mut add = |entries: &mut Vec<VersionInfo>, at: u64| {
            n += 1;
            entries.push(entry(n, at, VersionKind::Save));
            n
        };
        let recent: Vec<usize> = (0..5).map(|i| add(&mut entries, now - 29 * day + i)).collect();
        let hour_a_old = add(&mut entries, now - 40 * day + 60);
        let hour_a_new = add(&mut entries, now - 40 * day + 120);
        let hour_b = add(&mut entries, now - 40 * day + 3_600 + 60);
        let day_old = add(&mut entries, now - 100 * day + 3_600);
        let day_new = add(&mut entries, now - 100 * day + 7_200);
        let next_day = add(&mut entries, now - 99 * day + 3_600);
        let week = (((now - 400 * day) / day + 3) / 7 * 7 - 3) * day;
        let week_old = add(&mut entries, week + 3_600);
        let week_new = add(&mut entries, week + day);
        let ancient = add(&mut entries, now - 3_000 * day);
        let (kept, removed) = prune(&root, entries, now);
        let kept: BTreeSet<String> = kept.into_iter().map(|v| v.id).collect();
        let has = |n: usize| kept.contains(&format!("{n:032x}"));
        assert!(recent.iter().all(|&n| has(n)), "every version of the last 30 days stays");
        assert!(has(hour_a_new) && !has(hour_a_old) && has(hour_b), "one per hour between 30 and 60 days");
        assert!(has(day_new) && !has(day_old) && has(next_day), "one per day between 60 and 180 days");
        assert!(has(week_new) && !has(week_old), "one per week after 180 days");
        assert!(has(ancient), "a document left alone keeps its old history");
        assert!(removed.contains(&format!("{hour_a_old:032x}")));
        let many: Vec<VersionInfo> = (0..MAX_VERSIONS + 20).map(|i| entry(i + 1, now - u64::try_from(i).unwrap(), VersionKind::Save)).collect();
        let (kept, _) = prune(&root, many, now);
        assert_eq!(kept.len(), MAX_VERSIONS);
        let large: Vec<VersionInfo> =
            (0..100).map(|i| VersionInfo { size: 6 << 20, ..entry(i + 1, now - u64::try_from(i).unwrap(), VersionKind::Save) }).collect();
        let (kept, _) = prune(&root, large, now);
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
        let (kept, _) = prune(&root, list, now);
        assert!(kept.len() <= MAX_VERSIONS);
        assert_eq!(kept.last().map(|v| v.kind), Some(VersionKind::Original));
        let _ = fs::remove_dir_all(root);
    }

    // ---- damaged, foreign and legacy files ---------------------------------------------------

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn records_from_earlier_builds_migrate_and_damaged_files_cost_nothing() {
        let root = temp("legacy-index");
        let file = root.join("note.txt");
        let dir = document_dir(&root, &file);
        fs::create_dir_all(&dir).unwrap();
        // An earlier build's folder: `.bin` files and one shared index.json.
        let ids = ["0123456789abcdef0123456789abcdef", "fedcba9876543210fedcba9876543210"];
        let mut legacy = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            fs::write(dir.join(format!("{id}.bin")), format!("old {i}")).unwrap();
            legacy.push(
                json!({"id": id, "savedAt": crate::cmd::iso_from_unix_secs(1_800_000_000 + i as u64), "author": "Tester", "size": 5, "kind": "save"}),
            );
        }
        legacy.push(json!({"id": 7, "kind": "future"}));
        fs::write(dir.join(INDEX_FILE), serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(read_index(&dir).len(), 2, "read as it is, one bad entry costing only itself");
        let list = write(&root, &file, "new", VersionKind::Save, 1_800_000_100, 1_800_000_100);
        assert_eq!(list.len(), 3);
        assert!(!dir.join(INDEX_FILE).exists(), "the shared index is gone once each version has its own record");
        assert!(ids.iter().all(|id| dir.join(format!("{id}{META_SUFFIX}")).is_file()));
        // A damaged record or a version file nothing describes never costs the bytes.
        fs::write(dir.join(format!("{}{META_SUFFIX}", ids[0])), b"{ damaged").unwrap();
        let recovered = read_index(&dir);
        assert_eq!(recovered.len(), 3);
        assert!(dir.join(format!("{}.bin", ids[0])).is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_sync_apps_conflict_copies_are_ignored_and_left_alone() {
        let root = temp("conflicts");
        let file = root.join("Shared.docx");
        let picture = noise(2_000, 3);
        let list = save_bytes_at(&root, &file, &package(&body_xml(50, 0), &picture, None), 1_800_000_000);
        let dir = document_dir(&root, &file);
        let id = list[0].id.clone();
        // What OneDrive, Dropbox and iCloud leave beside a file both computers changed.
        let strays = [
            format!("{id} (1){PARTS_SUFFIX}"),
            format!("{id}-DESKTOP-4F2K9{PARTS_SUFFIX}"),
            format!("{id} (conflicted copy){META_SUFFIX}"),
            "index (1).json".to_owned(),
            "document-LAPTOP.json".to_owned(),
        ];
        for stray in &strays {
            fs::copy(manifest_path(&dir, &id), dir.join(stray)).unwrap();
        }
        let object = objects_in(&dir)[0].clone();
        fs::copy(object_path(&dir, &object), dir.join(OBJECTS_DIR).join(format!("{object} (1)"))).unwrap();
        assert_eq!(visible(&read_index(&dir)).len(), 1);
        let list = save_bytes_at(&root, &file, &package(&body_xml(50, 1), &picture, None), 1_800_000_060);
        assert_eq!(list.len(), 2);
        assert!(strays.iter().all(|stray| dir.join(stray).is_file()), "never deleted on a guess");
        assert!(dir.join(OBJECTS_DIR).join(format!("{object} (1)")).is_file());
        assert!(same_parts(&read_at(&root, &file, &id).unwrap(), &package(&body_xml(50, 0), &picture, None)));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn damaged_version_bytes_are_refused() {
        let root = temp("checksum");
        let file = root.join("note.txt");
        let list = write(&root, &file, "good", VersionKind::Save, 1_800_000_000, 1_800_000_000);
        let id = list[0].id.clone();
        fs::write(document_dir(&root, &file).join(format!("{id}.bin")), b"evil").unwrap();
        let store = Versions::start(root.clone(), Vec::new(), std::sync::Arc::new(|| {})).unwrap();
        assert!(store.read(&file, None, &id).is_err(), "same size, different bytes");
        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    // ---- what is kept before a save ------------------------------------------------------------

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
        assert_eq!(version_bytes(&s, &file, &first.id), original, "the bytes that were there before the first overwrite, byte for byte");
        let newest = &list[0];
        assert_eq!(newest.kind, VersionKind::Autosave);
        assert_eq!(newest.storage, VersionStorage::Parts);
        assert!(same_parts(&version_bytes(&s, &file, &newest.id), &fs::read(&file).unwrap()), "exactly the parts that went to disk");
        s.run("document.setText", &json!({"text": "edited again"})).unwrap();
        s.run("file.save", &json!({})).unwrap();
        let list = wait_versions(&s, &file, 3);
        assert_eq!(list.iter().filter(|v| v.kind == VersionKind::Original).count(), 1);
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
        let id = s.version_document_id().unwrap();
        // Another program writes the file, keeping the document id (as Word does).
        let mut theirs_doc = wordcraft_doc::Document::from_text("their edit");
        theirs_doc.set_custom_prop(meta::DOC_ID_PROP, &id);
        crate::io::save_path(&file, &theirs_doc).unwrap();
        let theirs = fs::read(&file).unwrap();
        s.run("document.setText", &json!({"text": "mine, later"})).unwrap();
        s.run("file.save", &json!({})).unwrap();
        let list = wait_versions(&s, &file, 3);
        let kept = list.iter().find(|v| v.kind == VersionKind::External).unwrap();
        assert_eq!(version_bytes(&s, &file, &kept.id), theirs);
        assert_eq!(s.version_document_id().as_deref(), Some(id.as_str()), "the same document");
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
        let versions = root.join("versions");
        fs::remove_dir_all(&versions).unwrap();
        fs::write(&versions, b"not a folder").unwrap();
        s.run("document.setText", &json!({"text": "saved anyway"})).unwrap();
        assert_eq!(s.run("file.save", &json!({"ifUnchanged": true})).unwrap()["saved"], true);
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

    // ---- identity ------------------------------------------------------------------------------

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_document_gets_an_id_at_its_first_save_and_keeps_it() {
        let root = temp("doc-id");
        let file = root.join("Plan.docx");
        let mut s = session_in(&root, "plan");
        assert!(s.version_document_id().is_none());
        s.run("file.save", &json!({"path": file.to_string_lossy()})).unwrap();
        let id = s.version_document_id().expect("an id at the first save");
        assert!(meta::document_id_ok(&id));
        assert_eq!(meta::file_document_id(&file).as_deref(), Some(id.as_str()), "written into docProps/custom.xml");
        // The package reader and writer keep it (a round trip through Word keeps custom
        // properties too: ECMA-376 Part 1 §22.3).
        let doc = crate::io::open_bytes("Plan.docx", &fs::read(&file).unwrap()).unwrap();
        assert_eq!(document_uuid(&doc).as_deref(), Some(id.as_str()));
        let again = crate::io::save_bytes_with("Plan.docx", &doc, None, &crate::io::ExportOptions::default()).unwrap();
        assert_eq!(meta::package_document_id(std::io::Cursor::new(again)).as_deref(), Some(id.as_str()));
        // Reopened and saved again: the same id and the same history.
        assert_eq!(wait_versions(&s, &file, 1).len(), 1);
        let mut t = session_in(&root, "");
        t.run("file.open", &json!({"path": file.to_string_lossy()})).unwrap();
        t.run("document.setText", &json!({"text": "plan, revised"})).unwrap();
        t.run("file.save", &json!({})).unwrap();
        assert_eq!(t.version_document_id().as_deref(), Some(id.as_str()));
        let list = wait_versions(&t, &file, 2);
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|v| v.document_id.as_deref() == Some(id.as_str())));
        assert!(root.join("versions").join(meta::id_bucket(&id)).is_dir(), "kept under the id");
        // Renaming keeps the id and the history; nothing moves.
        t.run("file.rename", &json!({"name": "Plan final"})).unwrap();
        let renamed = root.join("Plan final.docx");
        assert_eq!(wait_versions(&t, &renamed, 2).len(), 2);
        drop((s, t));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn removing_personal_information_keeps_the_id_and_the_author_out() {
        let root = temp("private");
        let file = root.join("Private.docx");
        let mut s = session_in(&root, "private");
        s.run("file.privacyOptions", &json!({"removePersonalInfo": true})).unwrap();
        s.run("file.save", &json!({"path": file.to_string_lossy()})).unwrap();
        assert!(meta::file_document_id(&file).is_none(), "no id in the file");
        assert!(s.version_document_id().is_none());
        let list = wait_versions(&s, &file, 1);
        assert_eq!(list.len(), 1, "the history is kept by path");
        assert!(list[0].author.is_empty());
        let details = list[0].details.as_ref().unwrap();
        assert!(details.initials.is_empty());
        assert!(details.properties.as_ref().is_some_and(|p| p.creator.is_empty() && p.last_modified_by.is_empty()));
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn save_as_makes_a_new_document_that_records_where_it_came_from() {
        let root = temp("save-as");
        let first = root.join("Draft.docx");
        let mut s = session_in(&root, "one");
        s.run("file.save", &json!({"path": first.to_string_lossy()})).unwrap();
        let source = s.version_document_id().unwrap();
        let source_version = wait_versions(&s, &first, 1)[0].id.clone();
        let copy = root.join("Copy.docx");
        s.run("file.saveAs", &json!({"path": copy.to_string_lossy()})).unwrap();
        let id = s.version_document_id().unwrap();
        assert_ne!(id, source, "a new file, a new id");
        let list = wait_versions(&s, &copy, 1);
        assert_eq!(list[0].kind, VersionKind::SaveAs);
        let origin = list[0].details.as_ref().and_then(|d| d.origin.clone()).unwrap();
        assert_eq!(origin.document_id.as_deref(), Some(source.as_str()));
        assert_eq!(origin.version_id.as_deref(), Some(source_version.as_str()));
        // The original keeps its own history.
        let store = s.version_store.as_ref().unwrap();
        assert_eq!(store_list(&store.roots(), &first, Some(&source), false, &[]).unwrap().len(), 1);
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
    fn a_copy_made_outside_the_app_gets_its_own_id_and_a_moved_file_keeps_its_history() {
        let root = temp("copy-id");
        let docs = root.join("docs");
        fs::create_dir_all(docs.join("archive")).unwrap();
        let original = docs.join("Plan.docx");
        let mut s = session_in(&root, "plan");
        s.run("file.save", &json!({"path": original.to_string_lossy()})).unwrap();
        let id = s.version_document_id().unwrap();
        let version = wait_versions(&s, &original, 1)[0].id.clone();
        // Copied in File Explorer while the original is still there.
        let copy = docs.join("Plan copy.docx");
        fs::copy(&original, &copy).unwrap();
        let mut t = session_in(&root, "");
        t.run("file.open", &json!({"path": copy.to_string_lossy()})).unwrap();
        assert_eq!(t.version_document_id().as_deref(), Some(id.as_str()), "the copy carries the id until it is saved");
        t.run("document.setText", &json!({"text": "copy"})).unwrap();
        t.run("file.save", &json!({"ifUnchanged": true})).unwrap();
        let copy_id = t.version_document_id().unwrap();
        assert_ne!(copy_id, id, "the copy gets an id of its own");
        assert_eq!(meta::file_document_id(&copy).as_deref(), Some(copy_id.as_str()));
        let copy_list = wait_versions(&t, &copy, 2);
        let newest = &copy_list[0];
        let origin = newest.details.as_ref().and_then(|d| d.origin.clone()).unwrap();
        assert_eq!((origin.document_id.as_deref(), origin.version_id.as_deref()), (Some(id.as_str()), Some(version.as_str())));
        assert_eq!(newest.details.as_ref().map(|d| d.trigger.as_str()), Some("copy"));
        assert_eq!(wait_versions(&s, &original, 1).len(), 1, "the original keeps its history alone");
        // Moved in File Explorer: same id, history found at once.
        let moved = docs.join("archive").join("Plan.docx");
        fs::rename(&original, &moved).unwrap();
        let mut u = session_in(&root, "");
        u.run("file.open", &json!({"path": moved.to_string_lossy()})).unwrap();
        assert_eq!(u.run("file.versions", &json!({})).unwrap().as_array().map(Vec::len), Some(1));
        u.run("document.setText", &json!({"text": "plan, moved"})).unwrap();
        u.run("file.save", &json!({})).unwrap();
        assert_eq!(u.version_document_id().as_deref(), Some(id.as_str()), "a move is not a copy");
        drop((s, t, u));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn history_kept_by_path_moves_under_the_id_at_the_first_save() {
        let root = temp("by-path");
        let file = root.join("Old.docx");
        crate::io::save_path(&file, &wordcraft_doc::Document::from_text("old")).unwrap();
        let versions = root.join("versions");
        // What an earlier build kept for this file, by path.
        let old = fs::read(&file).unwrap();
        let _ = save_bytes_at(&versions, &file, &old, 1_800_000_000);
        let by_path = document_dir(&versions, &file);
        assert!(by_path.is_dir());
        let mut s = session_in(&root, "");
        s.run("file.open", &json!({"path": file.to_string_lossy()})).unwrap();
        assert_eq!(s.run("file.versions", &json!({})).unwrap().as_array().map(Vec::len), Some(1), "listed before the first save");
        s.run("document.setText", &json!({"text": "new"})).unwrap();
        s.run("file.save", &json!({})).unwrap();
        let id = s.version_document_id().unwrap();
        let list = wait_versions(&s, &file, 2);
        assert_eq!(list.len(), 2, "{list:?}");
        assert!(!by_path.exists(), "the folder kept by path was taken over");
        assert!(versions.join(meta::id_bucket(&id)).is_dir());
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    // ---- folders --------------------------------------------------------------------------------

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_mirror_that_is_away_is_brought_up_to_date_when_it_returns() {
        let root = temp("mirror");
        let local = root.join("local");
        let away = root.join("usb");
        fs::write(&away, b"the drive is not there").unwrap();
        let file = root.join("Mirror.docx");
        let picture = noise(2_000, 5);
        let roots = vec![local.clone(), away.clone()];
        let save = |edit: usize, when: u64| {
            store_save_at(
                &roots,
                VersionWrite {
                    path: file.clone(),
                    bytes: package(&body_xml(80, edit), &picture, None),
                    author: "Tester".into(),
                    kind: VersionKind::Save,
                    saved_at: crate::cmd::iso_from_unix_secs(when),
                    session: None,
                    document_id: None,
                    details: VersionDetails::default(),
                    cloud: Vec::new(),
                },
                when,
            )
        };
        let (list, unavailable) = save(0, 1_800_000_000).unwrap();
        assert_eq!(list.len(), 1, "the save goes on without the mirror");
        assert_eq!(unavailable, vec![away.clone()]);
        // The drive comes back.
        fs::remove_file(&away).unwrap();
        fs::create_dir_all(&away).unwrap();
        let (list, unavailable) = save(1, 1_800_000_060).unwrap();
        assert!(unavailable.is_empty());
        let (a, b) = (document_dir(&local, &file), document_dir(&away, &file));
        assert_eq!(visible(&read_index(&b)).len(), 2, "the version it missed was copied over");
        assert_eq!(objects_in(&a), objects_in(&b));
        // Either folder alone gives every version back.
        fs::remove_dir_all(&a).unwrap();
        for v in &list {
            assert!(read_parts_version(&[a.clone(), b.clone()], &v.id).is_ok());
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn two_computers_writing_to_one_synced_folder_never_undo_each_other() {
        let root = temp("two-computers");
        let shared = root.join("OneDrive-history");
        let file = root.join("Joint.docx");
        let picture = noise(3_000, 9);
        let id = meta::new_document_id().unwrap();
        let write_from = |device: &str, edit: usize, when: u64| {
            store_save_at(
                std::slice::from_ref(&shared),
                VersionWrite {
                    path: file.clone(),
                    bytes: package(&body_xml(60, edit), &picture, None),
                    author: device.into(),
                    kind: VersionKind::Autosave,
                    saved_at: crate::cmd::iso_from_unix_secs(when),
                    session: Some(device.into()),
                    document_id: Some(id.clone()),
                    details: VersionDetails { device: device.into(), ..VersionDetails::default() },
                    cloud: Vec::new(),
                },
                when,
            )
            .unwrap()
            .0
        };
        let now = 1_800_000_000;
        write_from("Windows", 0, now);
        write_from("Mac", 1, now + 30);
        write_from("Windows", 2, now + 60);
        let list = write_from("Mac", 3, now + 90);
        assert_eq!(list.len(), 4, "each computer's session keeps its own versions");
        let dir = shared.join(meta::id_bucket(&id));
        assert!(!dir.join(INDEX_FILE).exists(), "no shared index to fight over");
        // An object that arrived before its manifest (the sync app is behind) is not deleted.
        let late = noise(4_000, 77);
        let late_hash = digest_hex(&late);
        fs::write(object_path(&dir, &late_hash), pack::encode_full(&late)).unwrap();
        let kept = visible(&read_index(&dir));
        collect_objects(&dir, &kept, now + 100);
        assert!(object_path(&dir, &late_hash).is_file(), "marked, not deleted");
        collect_objects(&dir, &kept, now + 100 + 6 * DAY_SECS);
        assert!(object_path(&dir, &late_hash).is_file(), "still inside the grace period");
        // Its manifest arrives: the mark goes and the object stays for good.
        let mut info = entry(99, now + 3 * DAY_SECS, VersionKind::Save);
        info.size = 1;
        let manifest = pack::Manifest {
            format: 1,
            info: VersionInfo { storage: VersionStorage::Parts, ..info.clone() },
            comment: String::new(),
            parts: vec![pack::ManifestPart {
                name: "late.bin".into(),
                method: pack::Method::Stored,
                date: 0,
                time: 0,
                mode: None,
                dir: false,
                size: late.len() as u64,
                sha256: late_hash.clone(),
            }],
        };
        fs::write(manifest_path(&dir, &info.id), serde_json::to_vec(&manifest).unwrap()).unwrap();
        let kept = visible(&read_index(&dir));
        collect_objects(&dir, &kept, now + 100 + 8 * DAY_SECS);
        assert!(object_path(&dir, &late_hash).is_file());
        // Without one, it goes after the grace period.
        fs::remove_file(manifest_path(&dir, &info.id)).unwrap();
        let kept = visible(&read_index(&dir));
        collect_objects(&dir, &kept, now + 100 + 9 * DAY_SECS);
        collect_objects(&dir, &kept, now + 100 + 17 * DAY_SECS);
        assert!(!object_path(&dir, &late_hash).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn history_folders_change_from_options_and_history_moves_along() {
        let root = temp("relocate");
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let file = root.join("Moving.docx");
        let mut s = session_in(&root, "moving");
        s.run("file.save", &json!({"path": file.to_string_lossy()})).unwrap();
        assert_eq!(wait_versions(&s, &file, 1).len(), 1);
        wait_idle(&mut s);
        let id = s.version_document_id().unwrap();
        let r = s.run("file.options", &json!({"versionsFolder": elsewhere.to_string_lossy(), "versionsExisting": "move"})).unwrap();
        assert_eq!(r["versionsFolder"], json!(elsewhere.to_string_lossy()));
        wait_idle(&mut s);
        assert!(elsewhere.join(meta::id_bucket(&id)).is_dir(), "moved to the new folder");
        assert!(!root.join("versions").join(meta::id_bucket(&id)).exists(), "and gone from the old one");
        assert_eq!(s.run("file.versions", &json!({})).unwrap().as_array().map(Vec::len), Some(1));
        // Mirror: both folders, every write to both.
        s.run("file.options", &json!({"versionsMirror": true, "versionsExisting": "copy"})).unwrap();
        wait_idle(&mut s);
        assert_eq!(s.version_folders().0, vec![root.join("versions"), elsewhere.clone()]);
        s.run("document.setText", &json!({"text": "mirrored"})).unwrap();
        s.run("file.save", &json!({})).unwrap();
        wait_idle(&mut s);
        let a = root.join("versions").join(meta::id_bucket(&id));
        let b = elsewhere.join(meta::id_bucket(&id));
        assert_eq!(visible(&read_index(&a)).len(), 2);
        assert_eq!(visible(&read_index(&b)).len(), 2);
        // Delete this document's history: in both folders.
        assert_eq!(s.run("file.versions", &json!({"delete": "document"})).unwrap()["deleted"], 2);
        assert!(!a.exists() && !b.exists());
        // Bad settings change nothing.
        assert!(s.run("file.options", &json!({"versionsFolder": "relative/path"})).is_err());
        assert!(s.run("file.options", &json!({"versionsExisting": "sideways", "versionsMirror": false})).is_err());
        assert!(s.run("file.versions", &json!({"delete": "everything"})).is_err());
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn deleting_all_history_empties_every_folder() {
        let root = temp("delete-all");
        let mut s = session_in(&root, "one");
        for name in ["A.docx", "B.docx", "c.rtf"] {
            s.run("file.save", &json!({"path": root.join(name).to_string_lossy()})).unwrap();
        }
        wait_idle(&mut s);
        assert!(s.run("file.versions", &json!({"delete": "all"})).unwrap()["deleted"].as_u64().unwrap() >= 3);
        let left: Vec<_> = fs::read_dir(root.join("versions")).unwrap().flatten().filter(|e| e.path().is_dir()).collect();
        assert!(left.is_empty(), "{left:?}");
        assert!(root.join("installation-id").is_file(), "the installation id is not history");
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    // ---- what a version records ------------------------------------------------------------------

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn versions_record_where_and_how_without_naming_the_computer_or_quoting_the_text() {
        let root = temp("details");
        let file = root.join("Contract.docx");
        let mut s = session_in(&root, "Confidential settlement clause number 4821 applies.");
        s.run("file.options", &json!({"deviceLabel": "Office laptop"})).unwrap();
        s.run("file.save", &json!({"path": file.to_string_lossy()})).unwrap();
        s.run("document.setText", &json!({"text": "Confidential settlement clause number 4821 applies, amended twice."})).unwrap();
        s.run("file.save", &json!({"trigger": "close"})).unwrap();
        let list = wait_versions(&s, &file, 2);
        let newest = &list[0];
        let d = newest.details.as_ref().unwrap();
        assert_eq!(d.trigger, "close");
        assert_eq!(d.parent.as_deref(), Some(list[1].id.as_str()), "the chain of versions");
        assert!(d.changed_parts.iter().any(|p| p == "word/document.xml"));
        assert_eq!(d.device, "Office laptop");
        assert_eq!(d.installation.len(), 32);
        assert_eq!(d.os, std::env::consts::OS);
        assert_eq!(d.app_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(d.provider, "local");
        assert!(d.path.ends_with("Contract.docx"));
        assert!(d.session_start.is_some() && d.editing_seconds.is_some());
        assert_eq!(d.initials, "TT");
        let before = d.before.as_ref().unwrap();
        assert_eq!(before.sha256.as_deref(), list[1].digest.as_deref(), "what was on disk before: the previous version");
        let stats = d.stats.as_ref().unwrap();
        assert_eq!(stats.paragraphs, 1);
        assert!(stats.words >= 7);
        assert_eq!((stats.words_inserted, stats.words_removed), (Some(3), Some(1)), "\"applies,\" \"amended\" \"twice.\" in, \"applies.\" out");
        assert_eq!(newest.author, "Tester");
        // The records hold no host name, no account name and no text of the document.
        let all = records(&root.join("versions")).join("\n").to_lowercase();
        assert!(!all.is_empty());
        for word in ["confidential", "settlement", "4821"] {
            assert!(!all.contains(word), "the text {word:?} is in a version record");
        }
        for key in ["COMPUTERNAME", "HOSTNAME", "USERNAME", "USER", "USERDOMAIN"] {
            if let Ok(value) = std::env::var(key)
                && value.len() > 2
                && value.to_lowercase() != "tester"
            {
                assert!(!all.contains(&value.to_lowercase()), "{key} ({value}) is in a version record");
            }
        }
        // The file itself carries only the random id.
        let custom = {
            let mut archive = zip::ZipArchive::new(fs::File::open(&file).unwrap()).unwrap();
            let mut xml = String::new();
            archive.by_name("docProps/custom.xml").unwrap().read_to_string(&mut xml).unwrap();
            xml
        };
        assert_eq!(custom.matches("<property").count(), 1);
        assert!(custom.contains(meta::DOC_ID_PROP));
        // `file.versions` shows the new fields as additions.
        let shown = s.run("file.versions", &json!({})).unwrap();
        let first = &shown.as_array().unwrap()[0];
        assert_eq!(first["details"]["trigger"], "close");
        assert!(first["documentId"].is_string() && first["stored"].is_u64() && first["size"].is_u64());
        // Device label back to the system's.
        let r = s.run("file.options", &json!({"deviceLabel": "auto"})).unwrap();
        assert!(r["deviceLabel"].is_null());
        assert!(!r["deviceLabelDefault"].as_str().unwrap_or("").is_empty());
        drop(s);
        let _ = fs::remove_dir_all(root);
    }

    // ---- open, restore and commands -------------------------------------------------------------

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
        wait_idle(&mut s);
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
        let file = root.join("Case.rtf");
        fs::write(&file, b"x").unwrap();
        let upper = root.join("CASE.RTF");
        assert_eq!(document_key(&file), document_key(&upper));
        let versions = root.join("versions");
        let legacy = versions.join(legacy_key(&file));
        assert_ne!(legacy, versions.join(document_key(&file)));
        let list = write(&versions, &file, "kept", VersionKind::Save, 1_800_000_000, 1_800_000_000);
        fs::rename(document_dir(&versions, &file), &legacy).unwrap();
        assert_eq!(
            visible(&read_index(&document_dir(&versions, &upper))).iter().map(|v| v.id.clone()).collect::<Vec<_>>(),
            list.iter().map(|v| v.id.clone()).collect::<Vec<_>>()
        );
        assert!(!legacy.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_file_kept_by_path_follows_a_rename_and_a_move() {
        let root = temp("by-path-move");
        let docs = root.join("docs");
        fs::create_dir_all(docs.join("archive")).unwrap();
        let file = docs.join("Letter.rtf");
        let mut s = session_in(&root, "letter");
        s.run("file.save", &json!({"path": file.to_string_lossy()})).unwrap();
        assert_eq!(wait_versions(&s, &file, 1).len(), 1);
        assert!(s.version_document_id().is_none(), "other formats carry no id");
        s.run("file.rename", &json!({"name": "Letter final"})).unwrap();
        let renamed = docs.join("Letter final.rtf");
        assert_eq!(wait_versions(&s, &renamed, 1).len(), 1, "the history follows the rename");
        wait_idle(&mut s);
        let moved = docs.join("archive").join("Letter final.rtf");
        fs::rename(&renamed, &moved).unwrap();
        let mut t = session_in(&root, "");
        t.run("file.open", &json!({"path": moved.to_string_lossy()})).unwrap();
        let mut found = Vec::new();
        for _ in 0..500 {
            found = t.version_infos();
            if !found.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(found.len(), 1, "the moved file's history is found by its bytes");
        drop((s, t));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn persistent_open_and_restore_keep_the_document_separate_and_undoable() {
        let root = temp("open-restore");
        let file = root.join("note.txt");
        let list = write(&root, &file, "previous", VersionKind::Save, 1_800_000_000, 1_800_000_000);
        let id = list.first().map(|entry| entry.id.clone()).unwrap_or_default();
        let start = |text: &str| {
            let mut s = crate::Session::new(wordcraft_doc::Document::from_text(text));
            s.path = Some(file.clone());
            s.version_store = Some(Versions::start(root.clone(), Vec::new(), std::sync::Arc::new(|| {})).unwrap());
            s
        };
        let mut opened = start("current");
        assert!(opened.run("file.versions", &json!({"open": id})).is_ok());
        assert_eq!(body(&opened), "previous");
        assert!(opened.path.is_none());
        let mut opened_restore = start("current");
        assert!(opened_restore.run("file.versions", &json!({"open": id.clone()})).is_ok());
        assert!(opened_restore.run("file.versions", &json!({"restore": id.clone()})).is_ok());
        assert_eq!(body(&opened_restore), "previous");
        assert!(opened_restore.run("edit.undo", &json!({})).is_ok());
        assert_eq!(body(&opened_restore), "current");
        let mut restored = start("current");
        assert!(restored.run("file.versions", &json!({"restore": list[0].id.clone()})).is_ok());
        assert_eq!(body(&restored), "previous");
        assert!(restored.run("edit.undo", &json!({})).is_ok());
        assert_eq!(body(&restored), "current");
        drop((opened, opened_restore, restored));
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
        s.run("file.versions", &json!({"restore": first[0].id.clone()})).unwrap();
        assert_eq!(body(&s), "first draft");
        let list = wait_versions(&s, &file, 3);
        assert_eq!(list.len(), 3, "{list:?}");
        assert_eq!(list[0].kind, VersionKind::Restore);
        assert_eq!(list[0].details.as_ref().map(|d| d.trigger.clone()), Some(format!("restore:{}", first[0].id)));
        assert!(same_text(&version_bytes(&s, &file, &list[0].id), &version_bytes(&s, &file, &first[0].id)), "with the restored content");
        let kept = &list[1];
        assert_eq!(kept.details.as_ref().map(|d| d.trigger.as_str()), Some("beforeRestore"));
        let (doc, _) = crate::io::open_bytes_with("Letter.docx", &version_bytes(&s, &file, &kept.id), None).unwrap();
        assert_eq!(doc.plain_text(wordcraft_doc::StoryRef::Body), "unsaved work");
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(body(&s), "unsaved work");
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
        assert!(session.run("file.versions", &json!({"delete": "all", "open": "x"})).is_err());
        assert!(session.run("file.save", &json!({"path": "x.docx", "trigger": "rm -rf /"})).is_err());
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

    // ---- parts and deltas --------------------------------------------------------------------

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
        assert!(first_stored > picture.len() as u64);
        assert!(second_stored < 5_000, "an edit costs a small delta, not the picture again: {second_stored} bytes");
        let dir = document_dir(&root, &file);
        assert_eq!(object_header(&dir, &digest_hex(&picture)).unwrap().0.depth, 0);
        assert!(same_parts(&read_at(&root, &file, &list[1].id).unwrap(), &first));
        assert!(same_parts(&read_at(&root, &file, &list[0].id).unwrap(), &second));
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
        assert_eq!(depths, expected);
        let list = visible(&read_index(&dir));
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
        let list = visible(&read_index(&dir));
        let body_of = |bytes: &[u8]| pack::split_zip(bytes).unwrap().0.into_iter().find(|p| p.meta.name == "word/document.xml").unwrap().meta.sha256;
        let third = object_path(&dir, &body_of(&packages[2]));
        let mut bytes = fs::read(&third).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x55;
        fs::write(&third, bytes).unwrap();
        assert!(same_parts(&read_at(&root, &file, &list[3].id).unwrap(), &packages[0]));
        assert!(same_parts(&read_at(&root, &file, &list[2].id).unwrap(), &packages[1]));
        assert!(read_at(&root, &file, &list[1].id).is_err());
        assert!(read_at(&root, &file, &list[0].id).is_err());
        fs::remove_file(object_path(&dir, &digest_hex(&picture))).unwrap();
        assert!(read_at(&root, &file, &list[3].id).is_err());
        let _ = save_bytes_at(&root, &file, &packages[2], now + 10);
        assert!(same_parts(&read_at(&root, &file, &list[1].id).unwrap(), &packages[2]));
        assert!(same_parts(&read_at(&root, &file, &list[0].id).unwrap(), &packages[3]));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn unused_parts_are_collected_after_the_grace_period_but_delta_bases_stay() {
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
        let kept: Vec<VersionInfo> = list.into_iter().filter(|v| v.id != first_id).collect();
        remove_versions(&dir, &[first_id.clone()].into_iter().collect());
        collect_objects(&dir, &kept, now + 2);
        assert!(objects_in(&dir).contains(&digest_hex(&only_first)), "marked, kept for the grace period");
        collect_objects(&dir, &kept, now + 2 + ORPHAN_GRACE_SECS);
        let objects = objects_in(&dir);
        assert!(!objects.contains(&digest_hex(&only_first)), "a part only the removed version had is collected");
        assert!(objects.contains(&body_first), "the base of a delta still in use stays");
        assert!(same_parts(&read_at(&root, &file, &kept[0].id).unwrap(), &second));
        fs::write(manifest_path(&dir, &kept[0].id), b"{ damaged").unwrap();
        let before = objects_in(&dir);
        collect_objects(&dir, &kept, now + 3 + 2 * ORPHAN_GRACE_SECS);
        assert_eq!(objects_in(&dir), before, "an unreadable manifest stops collection");
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
        let dir = document_dir(&root, &file);
        fs::create_dir_all(&dir).unwrap();
        let id = "0123456789abcdef0123456789abcdef";
        fs::write(dir.join(format!("{id}.bin")), &old).unwrap();
        let legacy = json!([{"id": id, "savedAt": crate::cmd::iso_from_unix_secs(now), "author": "Tester", "size": old.len(), "kind": "save", "digest": digest_hex(&old)}]);
        fs::write(dir.join(INDEX_FILE), serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(read_at(&root, &file, id).unwrap(), old);
        let newer = package(&body_xml(100, 1), &picture, None);
        let _ = save_bytes_at(&root, &file, &newer, now + 60);
        let list = visible(&read_index(&dir));
        let migrated = list.iter().find(|v| v.id == id).unwrap();
        assert_eq!(migrated.storage, VersionStorage::Parts, "moved to parts at the next save");
        assert!(!dir.join(format!("{id}.bin")).exists());
        assert!(same_parts(&read_at(&root, &file, id).unwrap(), &old));
        assert_eq!(migrated.digest.as_deref(), Some(digest_hex(&old).as_str()));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_original_and_files_that_are_not_zips_stay_byte_for_byte() {
        let root = temp("verbatim");
        let file = root.join("Original.docx");
        let original = package(&body_xml(50, 0), &noise(1_000, 17), None);
        let list = put(&root, &file, &original, VersionKind::Original, 1_800_000_000, 1_800_000_000, None);
        assert_eq!(list[0].storage, VersionStorage::Whole);
        assert_eq!(read_at(&root, &file, &list[0].id).unwrap(), original);
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
        assert_eq!(visible(&read_index(&dir)).len(), 20);
        let real = disk_bytes(&dir);
        assert!(real * 5 < logical, "20 versions take {real} bytes on disk instead of {logical}");
        let _ = fs::remove_dir_all(root);
    }

    /// The figure for the docs: 100 versions of a ~1.8 MB `.docx` with a picture, one paragraph
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
}
