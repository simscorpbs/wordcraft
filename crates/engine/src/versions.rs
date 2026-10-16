//! Local version history for desktop documents.
//!
//! The service backed histories of iCloud Drive, Google Drive and OneDrive remain owned by
//! those services.  This module is the small local safety net WordCraft keeps in the profile:
//! document bytes are written by a worker, beside a bounded JSON index.

use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::collections::BTreeSet;
#[cfg(not(target_arch = "wasm32"))]
use std::fs::{self, File, OpenOptions};
#[cfg(not(target_arch = "wasm32"))]
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, mpsc};

/// A version's origin.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VersionKind {
    Save,
    Autosave,
    Restore,
}

impl VersionKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::Autosave => "autosave",
            Self::Restore => "restore",
        }
    }
}

/// The metadata shown by `file.versions` and the Version History pane.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub id: String,
    pub saved_at: String,
    pub author: String,
    pub size: u64,
    pub kind: VersionKind,
}

/// The source document retained while a previous version is being viewed.
#[derive(Clone, Debug)]
pub struct OpenedVersion {
    pub(crate) path: PathBuf,
    pub(crate) info: VersionInfo,
}

#[cfg(not(target_arch = "wasm32"))]
const INDEX_FILE: &str = "index.json";
#[cfg(not(target_arch = "wasm32"))]
const MAX_INDEX_BYTES: u64 = 8 << 20;
/// A single document is never allowed to make the profile larger than 500 MiB.
pub const MAX_DOCUMENT_BYTES: u64 = 500 << 20;
pub const MAX_VERSIONS: usize = 100;
#[cfg(not(target_arch = "wasm32"))]
const AUTOSAVE_JOIN: u64 = 10 * 60;
#[cfg(not(target_arch = "wasm32"))]
const KEEP_ALL: u64 = 24 * 60 * 60;
#[cfg(not(target_arch = "wasm32"))]
const KEEP_DAILY: u64 = 30 * 24 * 60 * 60;
#[cfg(not(target_arch = "wasm32"))]
const MAX_ID_LEN: usize = 32;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct VersionWrite {
    path: PathBuf,
    bytes: Vec<u8>,
    author: String,
    kind: VersionKind,
    saved_at: String,
}

#[cfg(not(target_arch = "wasm32"))]
enum Job {
    Save(VersionWrite),
    List(PathBuf),
}

#[cfg(not(target_arch = "wasm32"))]
struct Event {
    path: PathBuf,
    list: Vec<VersionInfo>,
    result: Result<(), String>,
}

/// Desktop worker state.  Nothing in this struct is shared with the worker by reference, so a
/// slow disk never holds a borrow of the editing session.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Versions {
    tx: mpsc::SyncSender<Job>,
    rx: mpsc::Receiver<Event>,
    root: PathBuf,
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
fn new_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
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

/// A small deterministic hash avoids adding a crypto dependency just for a directory name.
pub fn document_key(path: &Path) -> String {
    let text = canonical_path(path).to_string_lossy();
    let mut hash = 0xcbf29ce484222325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(not(target_arch = "wasm32"))]
fn document_dir(root: &Path, path: &Path) -> PathBuf {
    root.join(document_key(path))
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
fn date_key(value: &str) -> &str {
    value.get(..10).unwrap_or(value)
}

#[cfg(not(target_arch = "wasm32"))]
fn read_index(dir: &Path) -> Vec<VersionInfo> {
    let path = dir.join(INDEX_FILE);
    let Ok(meta) = fs::symlink_metadata(&path) else { return Vec::new() };
    if !meta.is_file() || meta.len() > MAX_INDEX_BYTES {
        return Vec::new();
    }
    let Ok(file) = File::open(&path) else { return Vec::new() };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(MAX_INDEX_BYTES.saturating_add(1)).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > MAX_INDEX_BYTES {
        return Vec::new();
    }
    let Ok(mut list) = serde_json::from_slice::<Vec<VersionInfo>>(&bytes) else { return Vec::new() };
    list.retain(|v| {
        id_ok(&v.id) && v.size <= MAX_DOCUMENT_BYTES && parse_iso_secs(&v.saved_at).is_some() && dir.join(format!("{}.bin", v.id)).is_file()
    });
    list.sort_by(|a, b| parse_iso_secs(&b.saved_at).unwrap_or(0).cmp(&parse_iso_secs(&a.saved_at).unwrap_or(0)).then_with(|| b.id.cmp(&a.id)));
    list.truncate(MAX_VERSIONS);
    list
}

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
fn prune(dir: &Path, mut list: Vec<VersionInfo>, now: u64) -> Vec<VersionInfo> {
    list.sort_by(|a, b| parse_iso_secs(&b.saved_at).unwrap_or(0).cmp(&parse_iso_secs(&a.saved_at).unwrap_or(0)).then_with(|| b.id.cmp(&a.id)));
    let mut kept = Vec::new();
    let mut days = BTreeSet::new();
    for entry in list {
        let age = now.saturating_sub(parse_iso_secs(&entry.saved_at).unwrap_or(now));
        if age <= KEEP_ALL || (age <= KEEP_DAILY && days.insert(date_key(&entry.saved_at).to_string())) {
            kept.push(entry);
        }
        if kept.len() >= MAX_VERSIONS {
            break;
        }
    }
    while kept.iter().map(|v| v.size).sum::<u64>() > MAX_DOCUMENT_BYTES {
        if kept.len() <= 1 {
            break;
        }
        kept.pop();
    }
    let keep_ids: BTreeSet<&str> = kept.iter().map(|v| v.id.as_str()).collect();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if let Some(id) = name.strip_suffix(".bin")
                && id_ok(id)
                && !keep_ids.contains(id)
            {
                let _ = fs::remove_file(path);
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
    let replace = write.kind == VersionKind::Autosave
        && list
            .first()
            .is_some_and(|v| v.kind == VersionKind::Autosave && now.saturating_sub(parse_iso_secs(&v.saved_at).unwrap_or(now)) < AUTOSAVE_JOIN);
    let id = if replace { list.first().map(|v| v.id.clone()).ok_or("autosave history is inconsistent")? } else { new_id()? };
    write_version_file(&dir, &id, &write.bytes)?;
    let info = VersionInfo { id: id.clone(), saved_at: write.saved_at, author: write.author, size, kind: write.kind };
    if replace {
        if let Some(first) = list.first_mut() {
            *first = info;
        }
    } else {
        list.insert(0, info);
    }
    list = prune(&dir, list, now);
    save_index(&dir, &list)?;
    Ok(list)
}

#[cfg(not(target_arch = "wasm32"))]
fn store_list(root: &Path, path: &Path) -> Result<Vec<VersionInfo>, String> {
    let dir = document_dir(root, path);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    Ok(read_index(&dir))
}

#[cfg(not(target_arch = "wasm32"))]
impl Versions {
    pub fn start(root: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        private_dir(&root).map_err(|e| e.to_string())?;
        let (jobs, rx_jobs) = mpsc::sync_channel(8);
        let (events, rx) = mpsc::channel();
        let worker_root = root.clone();
        std::thread::Builder::new()
            .name("wordcraft-versions".into())
            .spawn(move || {
                while let Ok(job) = rx_jobs.recv() {
                    let (path, result) = match job {
                        Job::Save(write) => {
                            let path = write.path.clone();
                            let result = store_save(&worker_root, write);
                            (path, result)
                        }
                        Job::List(path) => {
                            let result = store_list(&worker_root, &path);
                            (path, result)
                        }
                    };
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
        Ok(Self { tx: jobs, rx, root, list: Vec::new(), ready: false, busy: false, error: None, current_path: None, alive: true })
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

    pub fn ensure_path(&mut self, path: &Path) {
        let canonical = canonical_path(path);
        if self.current_path.as_deref() == Some(canonical.as_path()) || self.busy {
            return;
        }
        self.current_path = Some(canonical.clone());
        self.list.clear();
        if let Err(error) = self.queue(Job::List(canonical)) {
            self.error = Some(error);
        }
    }

    pub fn save(&mut self, path: &Path, bytes: Vec<u8>, author: &str, kind: VersionKind, saved_at: String) {
        let path = canonical_path(path);
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            self.error = Some("version exceeds the 500 MiB document history limit".into());
            return;
        }
        if let Err(error) = self.queue(Job::Save(VersionWrite { path, bytes, author: author.to_string(), kind, saved_at })) {
            self.error = Some(error);
        }
    }

    pub fn read(&self, path: &Path, id: &str) -> Result<(VersionInfo, Vec<u8>), String> {
        if !id_ok(id) {
            return Err("invalid version id".into());
        }
        let dir = document_dir(&self.root, path);
        let list = read_index(&dir);
        let info = list.into_iter().find(|entry| entry.id == id).ok_or_else(|| "no such version".to_string())?;
        let file_path = dir.join(format!("{id}.bin"));
        let meta = fs::symlink_metadata(&file_path).map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.len() != info.size || meta.len() > MAX_DOCUMENT_BYTES {
            return Err("version bytes are missing or invalid".into());
        }
        let mut file = File::open(&file_path).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.take(MAX_DOCUMENT_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() as u64 != info.size {
            return Err("version bytes are truncated".into());
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

    pub(crate) fn record_version(&mut self, path: &Path, bytes: Vec<u8>, kind: VersionKind) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = self.version_store.as_mut() {
            store.save(path, bytes, &self.author, kind, crate::cmd::now_iso());
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (path, bytes, kind);
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
            let autosave = id == "file.save" && params.get("ifUnchanged").and_then(serde_json::Value::as_bool) == Some(true);
            let kind = self.take_version_kind(autosave);
            self.record_version(&path, bytes, kind);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (id, params, result);
    }

    pub(crate) fn read_version(&mut self, path: &Path, id: &str) -> Result<(VersionInfo, Vec<u8>), String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.versions_tick();
            let store = self.version_store.as_ref().ok_or_else(|| "local version history is unavailable".to_string())?;
            return store.read(path, id);
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
    fn write(root: &Path, file: &Path, text: &str, kind: VersionKind, when: u64, now: u64) -> Vec<VersionInfo> {
        store_save_at(
            root,
            VersionWrite {
                path: file.to_path_buf(),
                bytes: text.as_bytes().to_vec(),
                author: "Tester".into(),
                kind,
                saved_at: crate::cmd::iso_from_unix_secs(when),
            },
            now,
        )
        .unwrap()
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
        let third = write(&root, &file, "three", VersionKind::Autosave, 1_800_000_601, 1_800_000_601);
        assert_eq!(third.len(), 1, "the second write is still inside the ten-minute window");
        let fourth = write(&root, &file, "four", VersionKind::Autosave, 1_800_000_601 + AUTOSAVE_JOIN, 1_800_000_601 + AUTOSAVE_JOIN);
        assert_eq!(fourth.len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn pruning_keeps_daily_history_and_bounds_count() {
        let root = temp("prune");
        let file = root.join("note.docx");
        let now = 1_800_000_000;
        for i in 0..110u64 {
            let _ = write(&root, &file, "x", VersionKind::Save, now.saturating_sub(i * 900), now);
        }
        let list = read_index(&document_dir(&root, &file));
        assert!(list.len() <= MAX_VERSIONS);
        assert!(list.iter().map(|v| v.size).sum::<u64>() <= MAX_DOCUMENT_BYTES);
        let old = write(&root, &file, "old", VersionKind::Save, now.saturating_sub(KEEP_DAILY + 1), now);
        assert!(old.len() <= MAX_VERSIONS);

        let day_start = now / 86_400 * 86_400;
        let old_day = day_start.saturating_sub(KEEP_ALL + 60);
        let entries = vec![
            VersionInfo {
                id: "00000000000000000000000000000001".into(),
                saved_at: crate::cmd::iso_from_unix_secs(day_start.saturating_sub(60)),
                author: "Tester".into(),
                size: 1,
                kind: VersionKind::Save,
            },
            VersionInfo {
                id: "00000000000000000000000000000002".into(),
                saved_at: crate::cmd::iso_from_unix_secs(day_start.saturating_sub(KEEP_ALL + 60)),
                author: "Tester".into(),
                size: 1,
                kind: VersionKind::Save,
            },
            VersionInfo {
                id: "00000000000000000000000000000003".into(),
                saved_at: crate::cmd::iso_from_unix_secs(old_day.saturating_sub(60)),
                author: "Tester".into(),
                size: 1,
                kind: VersionKind::Save,
            },
            VersionInfo {
                id: "00000000000000000000000000000004".into(),
                saved_at: crate::cmd::iso_from_unix_secs(day_start.saturating_sub(KEEP_DAILY + 60)),
                author: "Tester".into(),
                size: 1,
                kind: VersionKind::Save,
            },
        ];
        let kept = prune(&root, entries, day_start);
        let old_day_key = date_key(&crate::cmd::iso_from_unix_secs(old_day)).to_owned();
        assert_eq!(kept.iter().filter(|v| date_key(&v.saved_at) == old_day_key.as_str()).count(), 1, "one version remains for an older day");
        assert!(!kept.iter().any(|v| v.id.ends_with("04")), "versions older than thirty days are removed");

        let large = (0..MAX_VERSIONS)
            .map(|i| VersionInfo {
                id: format!("{i:032x}"),
                saved_at: crate::cmd::iso_from_unix_secs(day_start.saturating_sub(u64::try_from(i).unwrap_or(0))),
                author: "Tester".into(),
                size: 6 << 20,
                kind: VersionKind::Save,
            })
            .collect();
        let kept = prune(&root, large, day_start);
        assert!(kept.len() <= MAX_VERSIONS);
        assert!(kept.iter().map(|v| v.size).sum::<u64>() <= MAX_DOCUMENT_BYTES);
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
        assert_eq!(opened.doc.plain_text(wordcraft_doc::StoryRef::Body), "previous");
        assert!(opened.path.is_none());
        assert!(opened.file_name.as_deref().is_some_and(|name| name.contains("(version ")));

        let mut restored = crate::Session::new(wordcraft_doc::Document::from_text("current"));
        restored.path = Some(file);
        restored.version_store = Some(Versions::start(root.clone(), std::sync::Arc::new(|| {})).unwrap());
        assert!(restored.run("file.versions", &json!({"restore": list[0].id})).is_ok());
        assert_eq!(restored.doc.plain_text(wordcraft_doc::StoryRef::Body), "previous");
        assert!(restored.run("edit.undo", &json!({})).is_ok());
        assert_eq!(restored.doc.plain_text(wordcraft_doc::StoryRef::Body), "current");
        assert_eq!(restored.version_next_kind, Some(VersionKind::Restore));
        drop(opened);
        drop(restored);
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
