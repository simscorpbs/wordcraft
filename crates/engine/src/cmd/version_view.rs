//! A previous version opened from Version History, shown the way Word shows one: it is read
//! only, it can be compared with the current document, and closing it brings the current
//! document back unchanged. Restoring it is `file.versions={"restore":id}`.
//!
//! WordCraft has one document per window, so the previous version takes the window's place
//! while it is shown and the current document waits in [`crate::versions::OpenedVersion`].

use serde_json::{Value, json};

use crate::{CmdError, CmdResult, CommandSpec, Session};

/// Why an edit is refused while a previous version is shown (the status line shows it).
pub const READ_ONLY: &str = "This is a previous version. Restore it or save a copy to make changes.";

/// What still works while a previous version is shown: the file commands (Restore, Save a copy,
/// Close, export, print), the view, moving the insertion point, selecting and copying. Undo and
/// Redo are refused as well: the version has no edits of its own to take back.
fn allowed(id: &str) -> bool {
    id.starts_with("file.") || id.starts_with("view.") || id.starts_with("caret.") || id.starts_with("select.") || id == "edit.copy"
}

/// Refuse command `id` (which changes the document when `mutates`) while a previous version is
/// shown. Called by [`Session::run`] before the command runs.
pub(crate) fn check_before(s: &Session, id: &str, mutates: bool) -> Result<(), CmdError> {
    if s.opened_version.is_none() || allowed(id) {
        return Ok(());
    }
    if mutates || matches!(id, "edit.undo" | "edit.undoTo" | "edit.redo" | "edit.repeat") {
        return Err(CmdError::Disabled(READ_ONLY.into()));
    }
    Ok(())
}

fn no_version(s: &Session) -> Option<&'static str> {
    if s.opened_version.is_none() { Some("no previous version is open") } else { None }
}

/// `file.closeVersion`: stop showing the previous version and go back to the current document,
/// exactly as it was (its unsaved changes, password and AutoSave's disk stamp included).
fn close_version(s: &mut Session, _: &Value) -> CmdResult {
    let Some(opened) = s.opened_version.take() else { return Err(CmdError::Disabled("no previous version is open".into())) };
    let id = opened.info.id.clone();
    s.set_document(opened.original_doc);
    s.path = Some(opened.path);
    s.file_name = None;
    s.password = opened.original_password;
    s.cloud.stamp = opened.original_stamp;
    s.cloud.conflict = opened.original_conflict;
    s.sel = opened.original_sel;
    s.clamp_selection();
    s.dirty = opened.original_dirty;
    Ok(json!({"closed": id, "path": s.path.as_ref().map(|p| p.to_string_lossy().into_owned())}))
}

/// `file.compareVersion`: show what changed between the previous version (the original) and the
/// current document (the revised one), as tracked changes. The comparison is shown in the
/// previous version's place and stays read only; Save a copy keeps it, and Restore and Close work
/// as before. Takes the comparison settings of `review.compare`.
fn compare_version(s: &mut Session, v: &Value) -> CmdResult {
    let Some(opened) = s.opened_version.as_ref() else { return Err(CmdError::Disabled("no previous version is open".into())) };
    let (path, id, current, password) = (opened.path.clone(), opened.info.id.clone(), opened.original_doc.clone(), opened.original_password.clone());
    let (_, bytes) = s.read_version(&path, &id).map_err(CmdError::Failed)?;
    let (version, _) =
        crate::io::open_bytes_with(&path.to_string_lossy(), &bytes, password.as_ref().map(crate::io::Password::as_str)).map_err(CmdError::Failed)?;
    let mut params = v.as_object().cloned().unwrap_or_default();
    for key in ["path", "text", "original", "showIn"] {
        params.remove(key);
    }
    let shown = s.doc.clone();
    s.doc = version;
    s.doc.ensure_nonempty();
    let result = match super::tools::compare_documents(s, &current, &Value::Object(params)) {
        Ok(result) => result,
        Err(e) => {
            s.doc = shown;
            return Err(e);
        }
    };
    s.touch();
    s.dirty = false;
    s.relayout();
    s.clamp_selection();
    let mut out = json!({"compared": id});
    if let (Some(out), Some(result)) = (out.as_object_mut(), result.as_object()) {
        out.extend(result.clone());
    }
    Ok(out)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        // Pure: going back to the current document and showing a comparison are not edits to
        // undo, and must not land in Repeat or a recorded macro.
        CommandSpec::new("file.closeVersion", "Close", "File › Info › Version History", close_version).when(no_version).pure(),
        CommandSpec::new("file.compareVersion", "Compare", "File › Info › Version History", compare_version)
            .params(r#"{"author"?: string, "caseChanges"?: bool, "whiteSpace"?: bool, "formatting"?: bool, "level"?: "word|character" (as review.compare)}"#)
            .when(no_version)
            .pure(),
    ]
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use crate::versions::VersionInfo;
    use wordcraft_doc::{Document, StoryRef};

    fn temp(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("wordcraft-version-view-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// A session on `root/note.docx` whose history has one version, "previous"; the document
    /// itself now reads "Unsaved current" and has unsaved changes.
    fn session(root: &std::path::Path) -> (Session, String) {
        let file = root.join("note.docx");
        let mut s = Session::new(Document::from_text("previous"));
        s.enable_versions(root.join("versions"), std::sync::Arc::new(|| {})).unwrap();
        s.run("file.save", &json!({"path": file.to_string_lossy()})).unwrap();
        let mut list: Vec<VersionInfo> = Vec::new();
        for _ in 0..1000 {
            list = s.version_infos();
            if !list.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let id = list.first().map(|v| v.id.clone()).unwrap();
        s.run("document.setText", &json!({"text": "current"})).unwrap();
        s.run("caret.docStart", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "Unsaved "})).unwrap();
        (s, id)
    }

    fn text(s: &Session) -> String {
        s.doc.plain_text(StoryRef::Body)
    }

    #[test]
    fn a_previous_version_is_read_only_and_closing_it_brings_the_document_back() {
        let root = temp("close");
        let (mut s, id) = session(&root);
        let path = s.path.clone();
        assert!(s.dirty);
        s.run("file.versions", &json!({"open": id})).unwrap();
        assert_eq!(text(&s), "previous");
        assert!(!s.dirty, "the previous version has no changes of its own");
        let e = s.run("text.insert", &json!({"text": "x"})).unwrap_err().to_string();
        assert!(e.contains(READ_ONLY), "{e}");
        assert!(s.run("edit.undo", &json!({})).is_err(), "Undo is refused too");
        assert!(s.run("format.bold", &json!({})).is_err());
        assert_eq!(text(&s), "previous", "nothing changed");
        // Moving and selecting still work.
        s.run("select.all", &json!({})).unwrap();
        s.run("caret.docEnd", &json!({})).unwrap();

        s.run("file.closeVersion", &json!({})).unwrap();
        assert!(s.previous_version().is_none());
        assert_eq!(text(&s), "Unsaved current", "the current document, with its unsaved edit");
        assert_eq!(s.path, path);
        assert!(s.dirty, "its changes are still unsaved, so AutoSave or Save Changes still sees them");
        s.run("text.insert", &json!({"text": "!"})).unwrap();
        assert!(s.run("file.closeVersion", &json!({})).is_err(), "nothing left to close");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn compare_shows_the_changes_and_restore_still_restores_the_version() {
        let root = temp("compare");
        let (mut s, id) = session(&root);
        s.run("file.versions", &json!({"open": id.clone()})).unwrap();
        let r = s.run("file.compareVersion", &json!({})).unwrap();
        assert_eq!(r["compared"], json!(id));
        assert!(r["changes"].as_u64().unwrap_or(0) > 0, "{r}");
        assert!(!s.doc.revisions.is_empty(), "the differences are tracked changes");
        assert!(s.previous_version().is_some(), "the comparison stays in the previous version's place");
        assert!(s.run("text.insert", &json!({"text": "x"})).is_err(), "and stays read only");
        s.run("file.versions", &json!({"restore": id})).unwrap();
        assert_eq!(text(&s), "previous");
        assert!(s.previous_version().is_none());
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(text(&s), "Unsaved current", "Restore is one undo step back to the current document");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn version_commands_need_an_open_version() {
        let mut s = Session::new(Document::from_text("x"));
        assert!(s.run("file.closeVersion", &json!({})).is_err());
        assert!(s.run("file.compareVersion", &json!({})).is_err());
        s.run("text.insert", &json!({"text": "y"})).unwrap();
    }
}
