//! Cloud AutoSave (iCloud Drive, Google Drive, OneDrive) through the folders their sync apps keep
//! (see `wordcraft-cloud` and `docs/cloud.md`).
//!
//! - `file.save` with `ifUnchanged` (what AutoSave sends) first checks that the document's file
//!   still holds what WordCraft last read or wrote. When another device or program changed it,
//!   nothing is written: the save reports the conflict and AutoSave waits for
//!   `file.resolveConflict` (keep both, keep mine, or take theirs).
//! - `file.cloud` says where the document is kept and whether a conflict is waiting.
//! - `file.saveToCloud` saves a document straight into a provider's folder, as Word offers to
//!   save to OneDrive when AutoSave is turned on for a new document.
//!
//! Cloud folders are known only once the host looks for them ([`CloudState::discover`]); the
//! desktop app does, headless sessions and tests don't, so a script never writes into someone's
//! cloud by accident.

use serde_json::{Value, json};
use wordcraft_cloud::{CloudRoot, DiskStamp, DiskState, Provider};

use crate::{CmdError, CmdResult, CommandSpec, Session, p};

/// What the session knows about the document's file and the cloud folders.
#[derive(Default)]
pub struct CloudState {
    /// The document's file as last read or written, for AutoSave's conflict check. `None` for a
    /// document with no file of its own (new, or opened from bytes).
    pub stamp: Option<DiskStamp>,
    /// A conflict AutoSave found and nobody has resolved yet: the file `changed` or went
    /// `missing`. AutoSave doesn't write while one waits.
    pub conflict: Option<DiskState>,
    /// The providers' folders on this computer, once the host has looked ([`Self::discover`]).
    pub roots: Vec<CloudRoot>,
}

impl CloudState {
    /// Look for the providers' folders on this computer (the desktop host does this once).
    pub fn discover(&mut self) {
        self.roots = wordcraft_cloud::discover(&wordcraft_cloud::Env::current());
    }

    /// The document's file changed hands (opened, replaced, new): forget the old file's stamp and
    /// any conflict about it.
    pub(crate) fn forget_file(&mut self) {
        self.stamp = None;
        self.conflict = None;
    }

    /// Remember the file at `path` as just read from it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn opened(&mut self, path: &std::path::Path) {
        self.forget_file();
        self.stamp = DiskStamp::read(path).ok();
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn opened(&mut self, _path: &std::path::Path) {
        self.forget_file();
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("file.cloud", "Cloud Status", "File › Info", status).pure(),
        CommandSpec::new("file.saveToCloud", "Save to Cloud", "File › Save As", save_to_cloud)
            .params(r#"{"provider"?: "icloud" | "gdrive" | "onedrive" (default: the first found, in that order), "name"?: string (file name without extension; default the document's name)}"#)
            .pure(),
        CommandSpec::new("file.resolveConflict", "Resolve Save Conflict", "File", resolve_conflict)
            .params(r#"{"keep": "both" (save this version beside the other as "Name (conflict copy).docx" and keep editing that) | "mine" (overwrite the other version) | "theirs" (reopen the file as it is now; unsaved changes here are discarded)}"#)
            .pure(),
    ]
}

/// The JSON for a provider's folder.
fn root_json(root: &CloudRoot) -> Value {
    json!({"provider": root.provider.id(), "name": root.provider.name(), "path": root.path, "account": root.account})
}

/// `file.cloud`: where the document is kept, the conflict waiting (if any) and the cloud folders
/// found on this computer.
fn status(s: &mut Session, _v: &Value) -> CmdResult {
    let location = s.path.as_deref().and_then(|path| wordcraft_cloud::locate(path, &s.cloud.roots));
    let not_downloaded = s.path.as_deref().is_some_and(|path| wordcraft_cloud::icloud_placeholder(path).is_some());
    Ok(json!({
        "provider": location.as_ref().map(|l| l.provider.id()),
        "name": location.as_ref().map(|l| l.provider.name()),
        "root": location.as_ref().map(|l| &l.root),
        "path": s.path,
        "conflict": s.cloud.conflict.map(DiskState::id),
        "notDownloaded": not_downloaded,
        "roots": s.cloud.roots.iter().map(root_json).collect::<Vec<_>>(),
        // The AutoSave switch for this document, the default for files without a choice, and
        // the choice remembered for this file (null: it follows the default). The app adds
        // whether AutoSave covers the file and, when not, why.
        "autosave": {"on": s.autosave, "default": s.prefs.autosave_by_default, "remembered": s.autosave_remembered()},
    }))
}

/// AutoSave's check before it writes the document's own file (`file.save` with `ifUnchanged`):
/// `Some(state)` when the file no longer holds what WordCraft last read or wrote, and the
/// conflict is recorded. A conflict already waiting stays until it is resolved.
pub(crate) fn conflict_before_save(s: &mut Session, path: &std::path::Path) -> Result<Option<DiskState>, CmdError> {
    if let Some(conflict) = s.cloud.conflict {
        return Ok(Some(conflict));
    }
    if s.path.as_deref() != Some(path) {
        return Ok(None);
    }
    let Some(stamp) = s.cloud.stamp.as_mut() else { return Ok(None) };
    let state = stamp.check(path).map_err(|e| CmdError::Failed(format!("{}: {e}", path.display())))?;
    if state == DiskState::Unchanged {
        return Ok(None);
    }
    s.cloud.conflict = Some(state);
    Ok(Some(state))
}

/// `file.saveToCloud`: save the document into a provider's documents folder under a name not
/// taken there, and keep it there (AutoSave can then cover it).
fn save_to_cloud(s: &mut Session, v: &Value) -> CmdResult {
    let wanted = match p::str(v, "provider") {
        Some(id) => Some(Provider::from_id(id).ok_or_else(|| CmdError::Params(format!("unknown provider `{id}`; use icloud, gdrive or onedrive")))?),
        None => None,
    };
    let root = s.cloud.roots.iter().find(|r| wanted.is_none_or(|p| r.provider == p)).cloned().ok_or_else(|| {
        CmdError::Failed(match wanted {
            Some(p) => format!("No {} folder was found on this computer. Install and sign in to its app, then try again.", p.name()),
            None => "No iCloud Drive, Google Drive or OneDrive folder was found on this computer.".to_string(),
        })
    })?;
    let stem = match p::str(v, "name") {
        Some(name) => name.to_string(),
        None => s
            .path
            .as_deref()
            .and_then(|p| p.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
            .or_else(|| s.file_name.clone())
            .unwrap_or_else(|| "Document".to_string()),
    };
    let path = wordcraft_cloud::unique_path(&root.save_dir(), &stem, "docx")
        .ok_or_else(|| CmdError::Failed(format!("Too many documents named {stem} in {}", root.provider.name())))?;
    let mut r = super::file::save(s, &json!({"path": path.to_string_lossy()}))?;
    if let Some(o) = r.as_object_mut() {
        o.insert("provider".into(), json!(root.provider.id()));
    }
    Ok(r)
}

/// `file.resolveConflict`: what to do once AutoSave found the file changed elsewhere.
fn resolve_conflict(s: &mut Session, v: &Value) -> CmdResult {
    let Some(conflict) = s.cloud.conflict else {
        return Err(CmdError::Disabled("no save conflict is waiting".into()));
    };
    let Some(path) = s.path.clone() else {
        s.cloud.conflict = None;
        return Err(CmdError::Disabled("no save conflict is waiting".into()));
    };
    let save = |s: &mut Session, path: &std::path::Path| super::file::save(s, &json!({"path": path.to_string_lossy()}));
    match p::req_str(v, "keep")? {
        "mine" => {
            s.cloud.conflict = None;
            let r = save(s, &path);
            if r.is_err() {
                s.cloud.conflict = Some(conflict);
            }
            r
        }
        "both" => {
            let copy = wordcraft_cloud::conflict_copy_path(&path, "conflict copy")
                .ok_or_else(|| CmdError::Failed(format!("Too many conflict copies of {}", path.display())))?;
            // The copy's history starts as a conflict copy of this document.
            s.version_next_kind = Some(crate::versions::VersionKind::ConflictCopy);
            let r = save(s, &copy);
            s.version_next_kind = None;
            let r = r?;
            // The session now edits the copy; the other version is left as it is.
            s.cloud.conflict = None;
            Ok(r)
        }
        "theirs" => {
            if conflict == DiskState::Missing {
                return Err(CmdError::Failed(format!("{} is no longer there to reopen", path.display())));
            }
            let mut params = json!({"path": path.to_string_lossy()});
            if let Some(pw) = &s.password {
                params["password"] = json!(pw.as_str());
            }
            super::file::open(s, &params)
        }
        other => Err(CmdError::Params(format!("`keep` must be both, mine or theirs, not `{other}`"))),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wordcraft_cloud::{CloudRoot, Provider};

    use crate::Session;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wc-engine-cloud-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn typed(s: &mut Session, text: &str) {
        s.run("text.insert", &json!({"text": text})).unwrap();
    }

    fn body(s: &Session) -> String {
        s.doc.plain_text(wordcraft_doc::StoryRef::Body)
    }

    /// Another device saves the file: AutoSave stops instead of overwriting it, and each way of
    /// resolving the conflict keeps what it says.
    #[test]
    fn autosave_never_overwrites_a_file_changed_elsewhere() {
        let dir = scratch("conflict");
        let path = dir.join("Notes.docx");
        let mut s = Session::new(wordcraft_doc::Document::new());
        typed(&mut s, "first");
        s.run("file.save", &json!({"path": path.to_string_lossy()})).unwrap();

        // Unchanged on disk: AutoSave writes.
        typed(&mut s, " here");
        let r = s.run("file.save", &json!({"ifUnchanged": true})).unwrap();
        assert_eq!(r["saved"], true);
        assert!(!s.dirty);

        // Another device's version arrives.
        let mut other = Session::new(wordcraft_doc::Document::new());
        typed(&mut other, "from the phone");
        other.run("file.save", &json!({"path": path.to_string_lossy()})).unwrap();
        let theirs = std::fs::read(&path).unwrap();

        typed(&mut s, " and more");
        let r = s.run("file.save", &json!({"ifUnchanged": true})).unwrap();
        assert_eq!(r["saved"], false);
        assert_eq!(r["conflict"], "changed");
        assert_eq!(std::fs::read(&path).unwrap(), theirs, "the other version is untouched");
        assert!(s.dirty);
        // It stays paused until resolved, even after more typing.
        typed(&mut s, "!");
        assert_eq!(s.run("file.save", &json!({"ifUnchanged": true})).unwrap()["conflict"], "changed");
        assert_eq!(s.run("file.cloud", &json!({})).unwrap()["conflict"], "changed");

        // Keep both: this version goes beside the other, and editing continues there.
        let r = s.run("file.resolveConflict", &json!({"keep": "both"})).unwrap();
        let copy = dir.join("Notes (conflict copy).docx");
        assert_eq!(r["path"], copy.to_string_lossy().as_ref());
        assert_eq!(s.path.as_deref(), Some(copy.as_path()));
        assert_eq!(std::fs::read(&path).unwrap(), theirs);
        assert!(s.run("file.cloud", &json!({})).unwrap()["conflict"].is_null());
        assert!(s.run("file.resolveConflict", &json!({"keep": "mine"})).is_err(), "nothing is waiting now");

        // Take theirs: the file is reopened as it is now.
        std::fs::copy(&path, &copy).unwrap();
        typed(&mut s, "lost");
        assert_eq!(s.run("file.save", &json!({"ifUnchanged": true})).unwrap()["conflict"], "changed");
        s.run("file.resolveConflict", &json!({"keep": "theirs"})).unwrap();
        assert!(body(&s).contains("from the phone"));
        assert!(!s.dirty);
        // …and AutoSave carries on from that version.
        typed(&mut s, "!");
        assert_eq!(s.run("file.save", &json!({"ifUnchanged": true})).unwrap()["saved"], true);

        // Keep mine: overwrite.
        std::fs::write(&copy, b"not a document").unwrap();
        typed(&mut s, "?");
        assert_eq!(s.run("file.save", &json!({"ifUnchanged": true})).unwrap()["conflict"], "changed");
        s.run("file.resolveConflict", &json!({"keep": "mine"})).unwrap();
        let reread = crate::io::open_path(&copy).unwrap();
        assert!(reread.plain_text(wordcraft_doc::StoryRef::Body).contains("!?from the phone"));

        // Deleted or moved elsewhere: missing, and there is nothing to reopen.
        std::fs::remove_file(&copy).unwrap();
        typed(&mut s, ".");
        assert_eq!(s.run("file.save", &json!({"ifUnchanged": true})).unwrap()["conflict"], "missing");
        assert!(!copy.exists(), "AutoSave doesn't bring back a file removed elsewhere");
        assert!(s.run("file.resolveConflict", &json!({"keep": "theirs"})).is_err());
        assert!(s.run("file.resolveConflict", &json!({"keep": "sideways"})).is_err());
        s.run("file.resolveConflict", &json!({"keep": "mine"})).unwrap();
        assert!(copy.exists());
    }

    /// A plain Save is the person's own decision and still writes; only AutoSave asks first.
    #[test]
    fn explicit_save_and_opened_files() {
        let dir = scratch("explicit");
        let path = dir.join("a.docx");
        let mut s = Session::new(wordcraft_doc::Document::new());
        typed(&mut s, "one");
        s.run("file.save", &json!({"path": path.to_string_lossy()})).unwrap();

        // A file opened here is stamped as read.
        let mut t = Session::new(wordcraft_doc::Document::new());
        t.run("file.open", &json!({"path": path.to_string_lossy()})).unwrap();
        typed(&mut t, "two");
        assert_eq!(t.run("file.save", &json!({"ifUnchanged": true})).unwrap()["saved"], true);
        // Now `s` is behind: its AutoSave stops, its explicit Save overwrites.
        typed(&mut s, "three");
        assert_eq!(s.run("file.save", &json!({"ifUnchanged": true})).unwrap()["conflict"], "changed");
        assert_eq!(s.run("file.save", &json!({})).unwrap()["saved"], true);
        assert!(s.run("file.cloud", &json!({})).unwrap()["conflict"].is_null());

        // A new document has no file to protect; saving a copy (PDF) doesn't move the stamp.
        s.run("file.new", &json!({})).unwrap();
        assert!(s.cloud.stamp.is_none());
    }

    #[test]
    fn save_to_cloud_uses_the_providers_documents_folder() {
        let dir = scratch("save");
        let icloud = dir.join("com~apple~CloudDocs");
        let onedrive = dir.join("OneDrive-Personal");
        std::fs::create_dir_all(icloud.join("Documents")).unwrap();
        std::fs::create_dir_all(&onedrive).unwrap();
        let mut s = Session::new(wordcraft_doc::Document::new());
        typed(&mut s, "draft");

        // Headless sessions know no cloud folders until the host looks for them.
        assert!(s.run("file.saveToCloud", &json!({})).is_err());

        s.cloud.roots = vec![
            CloudRoot { provider: Provider::ICloudDrive, path: icloud.clone(), account: None },
            CloudRoot { provider: Provider::OneDrive, path: onedrive.clone(), account: Some("Personal".into()) },
        ];
        s.run("file.rename", &json!({"name": "Plan"})).unwrap();
        let r = s.run("file.saveToCloud", &json!({})).unwrap();
        assert_eq!(r["provider"], "icloud");
        assert_eq!(s.path.as_deref(), Some(icloud.join("Documents/Plan.docx").as_path()));
        assert!(!s.dirty);
        let st = s.run("file.cloud", &json!({})).unwrap();
        assert_eq!(st["provider"], "icloud");
        assert_eq!(st["name"], "iCloud Drive");
        assert_eq!(st["roots"].as_array().unwrap().len(), 2);

        // Named provider and name; a taken name is numbered.
        s.run("file.saveToCloud", &json!({"provider": "onedrive", "name": "Plan"})).unwrap();
        s.run("file.saveToCloud", &json!({"provider": "onedrive", "name": "Plan"})).unwrap();
        assert_eq!(s.path.as_deref(), Some(onedrive.join("Plan 2.docx").as_path()));
        assert_eq!(s.run("file.cloud", &json!({})).unwrap()["provider"], "onedrive");
        assert!(s.run("file.saveToCloud", &json!({"provider": "gdrive"})).is_err());
        assert!(s.run("file.saveToCloud", &json!({"provider": "floppy"})).is_err());
        // Junk never panics.
        for junk in [json!(null), json!({"provider": 5, "name": []}), json!({"name": "../../x"})] {
            let _ = s.run("file.saveToCloud", &junk);
            let _ = s.run("file.resolveConflict", &junk);
            let _ = s.run("file.cloud", &junk);
        }
        // A name with separators stays inside the provider's folder.
        assert_eq!(s.path.as_deref().unwrap().parent(), Some(icloud.join("Documents").as_path()));
    }
}
