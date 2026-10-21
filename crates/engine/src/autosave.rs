//! AutoSave's switch for each file (see `docs/cloud.md`).
//!
//! AutoSave is on for every document whose file keeps everything, as the File › Options › Save
//! preference [`crate::Prefs::autosave_by_default`] says. Turning the switch the other way on a
//! file is remembered for that file, between runs, in a small JSON file in the settings folder
//! ([`crate::Session::enable_autosave_memory`]); turning it back to the default forgets it. The
//! choice follows the file through Rename, and Save As gives the new file the switch the document
//! had. Headless sessions remember only while they run; the browser has no files to remember.
//!
//! Whether AutoSave can write the file at all (a format that keeps everything, a disk stamp, no
//! conflict) is the front end's question; this module only keeps the user's choice.

use std::path::{Path, PathBuf};

#[cfg(not(target_arch = "wasm32"))]
use serde::{Deserialize, Serialize};

/// Most files remembered; the oldest choice is forgotten first.
pub const MAX_ENTRIES: usize = 2000;
/// Longest normalised path remembered, in bytes. A longer one keeps the default.
const MAX_KEY_BYTES: usize = 4096;
/// Largest memory file read back.
#[cfg(not(target_arch = "wasm32"))]
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// The memory file's format.
#[cfg(not(target_arch = "wasm32"))]
const VERSION: u32 = 1;

/// The choices the user made, oldest first, and where they are kept.
#[derive(Debug, Default)]
pub struct AutoSaveMemory {
    entries: Vec<(String, bool)>,
    /// `<settings_dir>/autosave.json` once the desktop host enabled it.
    #[cfg(not(target_arch = "wasm32"))]
    file: Option<PathBuf>,
    /// The last error reading or writing the file, if any.
    pub error: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    files: Vec<StoredEntry>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Serialize, Deserialize)]
struct StoredEntry {
    path: String,
    autosave: bool,
}

/// The key a file is remembered by: its canonical path, without the `\\?\` prefix Windows'
/// canonical form carries, with `\` separators and ignoring case on Windows and macOS (whose
/// usual file systems ignore it). `None` for a path too long to keep.
pub fn file_key(path: &Path) -> Option<String> {
    let canonical: PathBuf = crate::versions::canonical_path(path);
    let key = normalise(&canonical.to_string_lossy(), cfg!(windows), cfg!(any(windows, target_os = "macos")));
    (!key.is_empty() && key.len() <= MAX_KEY_BYTES).then_some(key)
}

/// [`file_key`]'s text rules without touching the disk: two spellings of one path (`C:/A.docx`,
/// `\\?\c:\a.docx`) give the same key on Windows. For lists such as Recent.
pub fn path_text_key(path: &str) -> String {
    normalise(path, cfg!(windows), cfg!(any(windows, target_os = "macos")))
}

/// [`file_key`]'s text rules, apart so tests can try Windows paths everywhere.
fn normalise(path: &str, windows: bool, fold_case: bool) -> String {
    let mut key = path.to_string();
    if windows {
        key = key.replace('/', "\\");
        if let Some(rest) = key.strip_prefix("\\\\?\\UNC\\") {
            key = format!("\\\\{rest}");
        } else if let Some(rest) = key.strip_prefix("\\\\?\\") {
            key = rest.to_string();
        }
    }
    if fold_case { key.to_lowercase() } else { key }
}

impl AutoSaveMemory {
    /// The switch the user left on `path`, when it differs from the default.
    pub fn get(&self, path: &Path) -> Option<bool> {
        let key = file_key(path)?;
        self.entries.iter().find(|(k, _)| *k == key).map(|(_, on)| *on)
    }

    /// Remember `on` for `path`; a choice that matches `default` is forgotten instead.
    pub fn remember(&mut self, path: &Path, on: bool, default: bool) {
        let Some(key) = file_key(path) else { return };
        let before = self.entries.len();
        self.entries.retain(|(k, _)| *k != key);
        if on == default && self.entries.len() == before {
            return;
        }
        if on != default {
            self.entries.push((key, on));
            self.trim();
        }
        self.persist();
    }

    /// The file at `old` is now at `new` (File › Info › Rename): its choice goes with it.
    pub fn rename(&mut self, old: &Path, new: &Path) {
        let (Some(old), Some(new)) = (file_key(old), file_key(new)) else { return };
        if old == new {
            return;
        }
        let Some(i) = self.entries.iter().position(|(k, _)| *k == old) else { return };
        let (_, on) = self.entries.remove(i);
        self.entries.retain(|(k, _)| *k != new);
        self.entries.push((new, on));
        self.persist();
    }

    /// How many files are remembered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn trim(&mut self) {
        if let Some(extra) = self.entries.len().checked_sub(MAX_ENTRIES).filter(|n| *n > 0) {
            self.entries.drain(..extra);
        }
    }

    /// Read the choices kept in `file` (a missing or damaged file remembers nothing) and keep
    /// writing them there.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(file: PathBuf) -> Self {
        let mut memory = AutoSaveMemory { entries: Vec::new(), file: None, error: None };
        match read_limited(&file) {
            Ok(Some(bytes)) => match serde_json::from_slice::<Stored>(&bytes) {
                Ok(stored) if stored.version == VERSION => {
                    for e in stored.files {
                        if !e.path.is_empty() && e.path.len() <= MAX_KEY_BYTES && !memory.entries.iter().any(|(k, _)| *k == e.path) {
                            memory.entries.push((e.path, e.autosave));
                        }
                    }
                    memory.trim();
                }
                Ok(_) => memory.error = Some(format!("{}: unknown version", file.display())),
                Err(e) => memory.error = Some(format!("{}: {e}", file.display())),
            },
            Ok(None) => {}
            Err(e) => memory.error = Some(e),
        }
        if let Some(e) = &memory.error {
            log::warn!("AutoSave choices not read: {e}");
        }
        memory.file = Some(file);
        memory
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn persist(&mut self) {
        let Some(file) = &self.file else { return };
        let stored =
            Stored { version: VERSION, files: self.entries.iter().map(|(path, on)| StoredEntry { path: path.clone(), autosave: *on }).collect() };
        let written = serde_json::to_vec_pretty(&stored).map_err(|e| e.to_string()).and_then(|bytes| write_atomic(file, &bytes));
        match written {
            Ok(()) => self.error = None,
            Err(e) => {
                log::warn!("AutoSave choices not saved to {}: {e}", file.display());
                self.error = Some(e);
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn persist(&mut self) {}
}

/// The file's bytes, `None` when there is no file, an error when it is too big or unreadable.
#[cfg(not(target_arch = "wasm32"))]
fn read_limited(file: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let f = match std::fs::File::open(file) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let mut bytes = Vec::new();
    f.take(MAX_FILE_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("{} is larger than {MAX_FILE_BYTES} bytes", file.display()));
    }
    Ok(Some(bytes))
}

/// Write `bytes` to a temporary file beside `path` and rename it over `path`, so an interrupted
/// write leaves the previous choices in place.
#[cfg(not(target_arch = "wasm32"))]
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "autosave.json".into());
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map_err(|e| format!("{}: {e}", path.display()))
}

impl crate::Session {
    /// Desktop host wiring: remember AutoSave choices in `file` (`<settings_dir>/autosave.json`)
    /// from now on, and give the open document the switch remembered for it.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn enable_autosave_memory(&mut self, file: PathBuf) {
        self.autosave_memory = AutoSaveMemory::load(file);
        self.refresh_autosave();
    }

    /// The switch remembered for the document's file, when it differs from the default.
    pub fn autosave_remembered(&self) -> Option<bool> {
        self.path.as_deref().and_then(|p| self.autosave_memory.get(p))
    }

    /// Set the document's switch from what is remembered for its file, else the default.
    pub fn refresh_autosave(&mut self) {
        self.autosave = self.autosave_remembered().unwrap_or(self.prefs.autosave_by_default);
    }

    /// The AutoSave switch for this document (`file.autosave`), remembered for its file.
    pub fn set_autosave(&mut self, on: bool) {
        self.autosave = on;
        if let Some(path) = self.path.clone() {
            self.autosave_memory.remember(&path, on, self.prefs.autosave_by_default);
        }
    }

    /// File › Options › Save: whether AutoSave is on for files with no choice of their own. The
    /// open document follows it unless its file has one.
    pub fn set_autosave_by_default(&mut self, on: bool) {
        self.prefs.autosave_by_default = on;
        self.refresh_autosave();
    }

    /// After a command: a document that was replaced (opened, new, reopened) takes its file's
    /// switch; Save As (and Save to Cloud, or keeping both after a conflict) gives the new file
    /// the switch the document had; a renamed file keeps its choice.
    pub(crate) fn follow_autosave_file(&mut self, id: &str, document: u64, path: Option<&Path>) {
        if self.document_id() != document {
            self.refresh_autosave();
            return;
        }
        if self.path.as_deref() == path {
            return;
        }
        match (id, path, self.path.clone()) {
            ("file.rename", Some(old), Some(new)) => self.autosave_memory.rename(old, &new),
            ("file.save" | "file.saveAs" | "file.saveToCloud" | "file.resolveConflict", _, Some(new)) => {
                self.autosave_memory.remember(&new, self.autosave, self.prefs.autosave_by_default);
            }
            _ => self.refresh_autosave(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn windows_paths_lose_the_verbatim_prefix_and_case() {
        assert_eq!(normalise(r"\\?\C:\Users\Ana\Doc.DOCX", true, true), r"c:\users\ana\doc.docx");
        assert_eq!(normalise("C:/Users/Ana/Doc.docx", true, true), r"c:\users\ana\doc.docx");
        assert_eq!(normalise(r"\\?\UNC\server\share\A.docx", true, true), r"\\server\share\a.docx");
        assert_eq!(normalise("/home/ana/Doc.docx", false, false), "/home/ana/Doc.docx");
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wordcraft-autosave-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn choices_persist_follow_renames_and_forget_the_default() {
        let dir = temp("memory");
        let store = dir.join("settings").join("autosave.json");
        let doc = dir.join("Report.docx");
        std::fs::write(&doc, b"x").unwrap();
        let mut m = AutoSaveMemory::load(store.clone());
        m.remember(&doc, false, true);
        assert_eq!(AutoSaveMemory::load(store.clone()).get(&doc), Some(false), "kept between runs");
        if cfg!(windows) {
            let upper = PathBuf::from(doc.to_string_lossy().to_uppercase());
            assert_eq!(m.get(&upper), Some(false), "case is ignored on Windows");
        }
        let renamed = dir.join("Final.docx");
        std::fs::rename(&doc, &renamed).unwrap();
        m.rename(&doc, &renamed);
        let m2 = AutoSaveMemory::load(store.clone());
        assert_eq!((m2.get(&doc), m2.get(&renamed)), (None, Some(false)));
        m.remember(&renamed, true, true);
        assert!(m.is_empty(), "turning it back on forgets the choice");
        assert!(AutoSaveMemory::load(store.clone()).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_damaged_or_huge_memory_file_remembers_nothing_and_the_list_is_bounded() {
        let dir = temp("damaged");
        let store = dir.join("autosave.json");
        std::fs::write(&store, b"{not json").unwrap();
        let m = AutoSaveMemory::load(store.clone());
        assert!(m.is_empty() && m.error.is_some());
        std::fs::write(&store, vec![b' '; (MAX_FILE_BYTES + 10) as usize]).unwrap();
        assert!(AutoSaveMemory::load(store.clone()).is_empty());
        let mut m = AutoSaveMemory::load(dir.join("fresh.json"));
        for i in 0..MAX_ENTRIES + 5 {
            m.remember(&dir.join(format!("f{i}.docx")), false, true);
        }
        assert_eq!(m.len(), MAX_ENTRIES);
        assert_eq!(m.get(&dir.join("f0.docx")), None, "the oldest choice went first");
        assert_eq!(m.get(&dir.join(format!("f{}.docx", MAX_ENTRIES + 4))), Some(false));
        assert_eq!(AutoSaveMemory::load(dir.join("fresh.json")).len(), MAX_ENTRIES);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_session_switch_follows_open_save_as_and_rename() {
        let dir = temp("session");
        let store = dir.join("autosave.json");
        let a = dir.join("a.docx");
        let mut s = crate::Session::new(wordcraft_doc::Document::new());
        s.enable_autosave_memory(store.clone());
        assert!(s.autosave, "on by default");
        s.run("file.save", &json!({"path": a.to_string_lossy()})).unwrap();
        assert_eq!(s.run("file.autosave", &json!({"value": false})).unwrap(), json!({"value": false}));
        // Save As: the new file gets the switch the document had.
        let b = dir.join("b.docx");
        s.run("file.save", &json!({"path": b.to_string_lossy()})).unwrap();
        assert!(!s.autosave);
        let mut t = crate::Session::new(wordcraft_doc::Document::new());
        t.enable_autosave_memory(store.clone());
        t.run("file.open", &json!({"path": a.to_string_lossy()})).unwrap();
        assert!(!t.autosave, "remembered off for a.docx");
        t.run("file.rename", &json!({"name": "c"})).unwrap();
        t.run("file.new", &json!({})).unwrap();
        assert!(t.autosave, "a new document has the default");
        t.run("file.open", &json!({"path": dir.join("c.docx").to_string_lossy()})).unwrap();
        assert!(!t.autosave, "the renamed file kept its choice");
        let cloud = t.run("file.cloud", &json!({})).unwrap();
        assert_eq!(cloud["autosave"], json!({"on": false, "default": true, "remembered": false}));
        // The default off: files with no choice come off, a file turned on stays on.
        t.set_autosave_by_default(false);
        t.run("file.open", &json!({"path": b.to_string_lossy()})).unwrap();
        assert!(!t.autosave);
        t.run("file.autosave", &json!({"value": true})).unwrap();
        let mut u = crate::Session::new(wordcraft_doc::Document::new());
        u.prefs.autosave_by_default = false;
        u.enable_autosave_memory(store);
        u.run("file.open", &json!({"path": b.to_string_lossy()})).unwrap();
        assert!(u.autosave, "turned on with the default off, and remembered");
        let _ = std::fs::remove_dir_all(dir);
    }
}
