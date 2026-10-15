//! Cloud storage for AutoSave: iCloud Drive, Google Drive and OneDrive through the folders their
//! sync apps keep on the computer.
//!
//! WordCraft doesn't talk to the services itself. Each one's own app (iCloud for Mac or Windows,
//! Google Drive for desktop, OneDrive) keeps a folder in step with the cloud; a document saved
//! there is uploaded by that app and comes down on the person's other devices. What this crate
//! adds is what a word processor needs on top of a plain folder:
//!
//! - [`discover`] finds those folders on this computer ([`Env`] says where to look), and
//!   [`locate`] says which service, if any, keeps a given document.
//! - [`DiskStamp`] remembers the file as WordCraft last read or wrote it, so AutoSave can see
//!   that another device changed it in the meantime and stop instead of overwriting that work.
//! - [`conflict_copy_path`] and [`unique_path`] name the extra file kept when both versions stay,
//!   and a new document saved straight to the cloud.
//! - [`icloud_placeholder`] spots an iCloud Drive file that is in the cloud only, not downloaded.
//!
//! Nothing here touches the network or needs an account, and nothing runs in the browser build
//! (it has no folders: [`discover`] finds none there).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod discover;
mod names;
mod stamp;

pub use discover::{Env, Os, discover};
pub use names::{conflict_copy_path, unique_path};
pub use stamp::{DiskStamp, DiskState, MAX_COMPARE_BYTES};

use std::path::{Component, Path, PathBuf};

/// A cloud storage service whose sync app keeps a folder on the computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Provider {
    ICloudDrive,
    GoogleDrive,
    OneDrive,
}

impl Provider {
    /// Every provider, in the order a new document picks one when none is named.
    pub const ALL: [Provider; 3] = [Provider::ICloudDrive, Provider::GoogleDrive, Provider::OneDrive];

    /// The id commands and scripts use: `icloud`, `gdrive` or `onedrive`.
    pub fn id(self) -> &'static str {
        match self {
            Provider::ICloudDrive => "icloud",
            Provider::GoogleDrive => "gdrive",
            Provider::OneDrive => "onedrive",
        }
    }

    /// The service's own name, as people know it (not translated).
    pub fn name(self) -> &'static str {
        match self {
            Provider::ICloudDrive => "iCloud Drive",
            Provider::GoogleDrive => "Google Drive",
            Provider::OneDrive => "OneDrive",
        }
    }

    /// The provider an id names ([`Self::id`]; also a few spellings people type).
    pub fn from_id(id: &str) -> Option<Provider> {
        match id.trim().to_ascii_lowercase().as_str() {
            "icloud" | "iclouddrive" | "icloud drive" => Some(Provider::ICloudDrive),
            "gdrive" | "google" | "googledrive" | "google drive" => Some(Provider::GoogleDrive),
            "onedrive" | "one drive" => Some(Provider::OneDrive),
            _ => None,
        }
    }
}

/// A folder a provider's sync app keeps on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudRoot {
    pub provider: Provider,
    /// The synchronized folder; everything below it is in the cloud.
    pub path: PathBuf,
    /// The account or organization the folder belongs to, when the folder's name says
    /// (`GoogleDrive-name@example.com`, `OneDrive-Contoso`).
    pub account: Option<String>,
}

impl CloudRoot {
    /// Where a new document saved to this provider goes: the provider's documents folder when it
    /// has one (`Documents` in iCloud Drive and OneDrive, My Drive in Google Drive), else the root.
    pub fn save_dir(&self) -> PathBuf {
        let candidates: &[&str] = match self.provider {
            Provider::ICloudDrive | Provider::OneDrive => &["Documents"],
            Provider::GoogleDrive => discover::MY_DRIVE_NAMES,
        };
        candidates.iter().map(|name| self.path.join(name)).find(|dir| dir.is_dir()).unwrap_or_else(|| self.path.clone())
    }
}

/// Where a document is kept: the provider and the folder that holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudLocation {
    pub provider: Provider,
    /// The synchronized folder the document is inside.
    pub root: PathBuf,
}

/// Which provider keeps `path`, if any: the innermost of `roots` that contains it, else what
/// the path's own folder names say (`…/Mobile Documents/com~apple~CloudDocs/…`, `…\\iCloudDrive\\…`,
/// `…/CloudStorage/GoogleDrive-…/…`, `…/CloudStorage/OneDrive-…/…`), so a document still counts
/// when discovery missed its folder.
pub fn locate(path: &Path, roots: &[CloudRoot]) -> Option<CloudLocation> {
    roots
        .iter()
        .filter(|root| contains(&root.path, path))
        .max_by_key(|root| root.path.components().count())
        .map(|root| CloudLocation { provider: root.provider, root: root.path.clone() })
        .or_else(|| locate_by_name(path))
}

/// Whether `path` lies inside `root`, comparing whole components, without regard to case on
/// macOS and Windows (whose usual file systems ignore it).
fn contains(root: &Path, path: &Path) -> bool {
    let root: Vec<Component<'_>> = root.components().collect();
    let path: Vec<Component<'_>> = path.components().collect();
    root.len() < path.len() && root.iter().zip(&path).all(|(a, b)| same_component(a.as_os_str(), b.as_os_str()))
}

fn same_component(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn locate_by_name(path: &Path) -> Option<CloudLocation> {
    let mut root = PathBuf::new();
    let mut previous: Option<String> = None;
    // The last component is the document itself: only its folders say where it is kept.
    let folders = path.parent()?;
    for component in folders.components() {
        root.push(component.as_os_str());
        let name = component.as_os_str().to_string_lossy();
        let provider = match previous.as_deref() {
            Some("Mobile Documents") if name == "com~apple~CloudDocs" => Some(Provider::ICloudDrive),
            // iCloud for Windows keeps the folder in the user's profile.
            _ if name == "iCloudDrive" => Some(Provider::ICloudDrive),
            Some("CloudStorage") if name.starts_with("GoogleDrive-") => Some(Provider::GoogleDrive),
            Some("CloudStorage") if name.starts_with("OneDrive-") || name == "OneDrive" => Some(Provider::OneDrive),
            _ => None,
        };
        if let Some(provider) = provider {
            return Some(CloudLocation { provider, root });
        }
        previous = Some(name.into_owned());
    }
    None
}

/// The placeholder iCloud Drive leaves for a file kept in the cloud only (`.Name.docx.icloud`
/// beside where `Name.docx` would be), when `path` itself is missing and that placeholder exists.
/// Opening such a file needs it downloaded first (Finder's Download Now, or opening it there).
pub fn icloud_placeholder(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    let placeholder = path.with_file_name(format!(".{name}.icloud"));
    (!path.exists() && placeholder.is_file()).then_some(placeholder)
}

#[cfg(test)]
mod tests;
