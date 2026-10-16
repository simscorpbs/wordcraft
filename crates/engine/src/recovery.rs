//! Local desktop recovery. Encoding and file I/O run on a worker, outside typing.
use crate::{CmdError, CmdResult, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryInfo {
    pub id: String,
    pub name: String,
    pub saved_at: String,
}

#[cfg(not(target_arch = "wasm32"))]
mod desktop;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use desktop::Recovery;

impl Session {
    /// The desktop host supplies the profile folder and a thread-safe wakeup.
    /// Headless sessions and the browser never write recovery files implicitly.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn enable_recovery(&mut self, root: std::path::PathBuf, wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Result<(), String> {
        self.recovery = Some(Recovery::start(root, wake)?);
        Ok(())
    }
    pub fn recovery_documents(&self) -> Vec<RecoveryInfo> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = &self.recovery {
            return r.documents.clone();
        }
        Vec::new()
    }
    pub fn recovery_ready(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = &self.recovery {
            return r.ready;
        }
        false
    }
    pub fn recovery_busy(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = &self.recovery {
            return r.busy;
        }
        false
    }
    pub fn recovery_error(&self) -> Option<&str> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = &self.recovery {
            return r.error.as_deref();
        }
        None
    }
    /// A delayed restore can't replace edits made while it was loading. Restores have no path.
    pub fn recovery_tick(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(mut r) = self.recovery.take() else { return };
            if let Some((snapshot, expected)) = r.poll() {
                if expected == (self.document_id(), self.rev()) && self.password.is_none() {
                    let id = snapshot.info.id;
                    self.set_document(snapshot.document);
                    self.path = None;
                    self.dirty = true;
                    r.adopt(id, self.document_id(), self.rev());
                } else {
                    r.error = Some("Recovery cancelled: the document changed while the copy was loading.".into());
                }
            }
            if let Err(e) = r.tick(self, std::time::Instant::now()) {
                r.error = Some(e);
            }
            if let Some(e) = &r.error {
                self.status = e.clone();
            }
            self.recovery = Some(r);
        }
    }
    pub fn recovery_wait(&self) -> Option<std::time::Duration> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = &self.recovery
            && self.dirty
            && self.password.is_none()
            && !r.busy
        {
            return r.wait(self.document_id(), self.rev());
        }
        None
    }
    /// Only an explicit protect/save action waits for the worker and removes plaintext copies.
    /// A failure prevents applying the password; typing never waits.
    pub(crate) fn clear_recovery_before_password(&mut self) -> Result<(), CmdError> {
        let document = self.document_id();
        self.clear_recovery_for_document(document).map_err(CmdError::Failed)
    }

    /// Remove the current document's recovery generations once it has been saved.
    ///
    /// Recovery records for another document remain available. The worker is allowed to finish a
    /// pending snapshot before it removes the record, so a save followed by close cannot leave a
    /// stale copy behind.
    pub fn clear_recovery_if_clean(&mut self) -> Result<(), String> {
        if self.dirty {
            return Ok(());
        }
        let document = self.document_id();
        self.clear_recovery_for_document(document)
    }

    fn clear_recovery_for_document(&mut self, document: u64) -> Result<(), String> {
        #[cfg(target_arch = "wasm32")]
        let _ = document;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = &mut self.recovery
            && r.document_is(document)
        {
            r.purge()?;
        }
        Ok(())
    }
    pub(crate) fn recover(&mut self, v: &Value) -> CmdResult {
        let restore = v.get("restore");
        let discard = v.get("discard");
        if restore.is_some() && discard.is_some() {
            return Err(CmdError::Params("choose restore or discard".into()));
        }
        if restore.is_some() || discard.is_some() || v.get("refresh").is_some() {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let expected = (self.document_id(), self.rev());
                let r = self.recovery.as_mut().ok_or_else(|| CmdError::Disabled("local recovery requires the desktop host".into()))?;
                if let Some(id) = restore {
                    r.restore(id.as_str().ok_or_else(|| CmdError::Params("restore must be a recovery id".into()))?, expected)
                        .map_err(CmdError::Failed)?;
                } else if let Some(id) = discard {
                    r.discard(id.as_str().ok_or_else(|| CmdError::Params("discard must be a recovery id".into()))?, expected)
                        .map_err(CmdError::Failed)?;
                } else if v.get("refresh").and_then(Value::as_bool) == Some(true) {
                    r.refresh().map_err(CmdError::Failed)?;
                } else {
                    return Err(CmdError::Params("refresh must be true".into()));
                }
            }
            #[cfg(target_arch = "wasm32")]
            return Err(CmdError::Disabled("local recovery is available on desktop only".into()));
        }
        Ok(
            json!({"enabled": self.recovery_ready(), "busy": self.recovery_busy(), "documents": self.recovery_documents(), "error": self.recovery_error()}),
        )
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn settle(session: &mut Session) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            session.recovery_tick();
            if session.recovery_ready() && !session.recovery_busy() {
                return;
            }
            assert!(Instant::now() < end, "recovery did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn clean_document_recovery_is_removed_but_dirty_document_is_kept() {
        let root = std::env::temp_dir().join(format!("wordcraft-recovery-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut session = Session::new(wordcraft_doc::Document::new());
        session.enable_recovery(root.clone(), Arc::new(|| {})).unwrap();
        settle(&mut session);
        session.run("text.insert", &json!({"text": "recover me"})).unwrap();

        let mut recovery = session.recovery.take().unwrap();
        recovery.tick(&session, Instant::now() + Duration::from_secs(11)).unwrap();
        session.recovery = Some(recovery);
        settle(&mut session);
        assert_eq!(session.recovery_documents().len(), 1);

        session.clear_recovery_if_clean().unwrap();
        assert_eq!(session.recovery_documents().len(), 1);
        let saved = root.join("draft.docx");
        session.run("file.save", &json!({"path": saved.to_string_lossy()})).unwrap();
        assert!(!session.dirty);
        session.clear_recovery_if_clean().unwrap();
        settle(&mut session);
        assert!(session.recovery_documents().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
