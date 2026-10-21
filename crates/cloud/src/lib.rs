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
    let inside = |path: &Path| {
        roots
            .iter()
            .filter(|root| contains(&root.path, path))
            .max_by_key(|root| root.path.components().count())
            .map(|root| CloudLocation { provider: root.provider, root: root.path.clone() })
    };
    inside(path).or_else(|| long_names(path).and_then(|long| inside(&long))).or_else(|| locate_by_name(path))
}

/// On Windows, a path spelled with short 8.3 names (`C:\Users\ANA~1\ONEDRI~1\a.docx`, as some
/// programs pass files on) under its long names, so it is found inside its root as well. Only such
/// paths touch the disk.
#[cfg(windows)]
fn long_names(path: &Path) -> Option<PathBuf> {
    let short = path.components().any(|c| matches!(c, Component::Normal(name) if name.to_string_lossy().contains('~')));
    if short { std::fs::canonicalize(path).ok() } else { None }
}

#[cfg(not(windows))]
fn long_names(_path: &Path) -> Option<PathBuf> {
    None
}

/// Whether `path` lies inside `root`, comparing whole components, without regard to case on
/// macOS and Windows (whose usual file systems ignore it). On Windows a verbatim prefix names the
/// same drive or share as the ordinary one: `\\?\C:\Users\…` is inside `C:\Users`.
fn contains(root: &Path, path: &Path) -> bool {
    let root: Vec<Component<'_>> = root.components().collect();
    let path: Vec<Component<'_>> = path.components().collect();
    root.len() < path.len() && root.iter().zip(&path).all(|(a, b)| same_part(a, b))
}

fn same_part(a: &Component<'_>, b: &Component<'_>) -> bool {
    match (a, b) {
        (Component::Prefix(a), Component::Prefix(b)) => same_prefix(a, b),
        _ => same_component(a.as_os_str(), b.as_os_str()),
    }
}

/// `C:` and `\\?\C:` are one drive, `\\server\share` and `\\?\UNC\server\share` one share (Windows
/// names ignore case). Other prefixes (`\\.\COM1`, `\\?\Volume{…}`) must match as written.
fn same_prefix(a: &std::path::PrefixComponent<'_>, b: &std::path::PrefixComponent<'_>) -> bool {
    use std::path::Prefix;
    let drive = |p: Prefix<'_>| match p {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => Some(letter.to_ascii_uppercase()),
        _ => None,
    };
    let share = |p: Prefix<'_>| match p {
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            Some((server.to_string_lossy().to_lowercase(), share.to_string_lossy().to_lowercase()))
        }
        _ => None,
    };
    match (drive(a.kind()), drive(b.kind()), share(a.kind()), share(b.kind())) {
        (Some(x), Some(y), _, _) => x == y,
        (_, _, Some(x), Some(y)) => x == y,
        _ => same_component(a.as_os_str(), b.as_os_str()),
    }
}

/// A folder name compared as the file system does: without regard to case on macOS and Windows.
fn named(name: &str, wanted: &str) -> bool {
    if cfg!(any(target_os = "macos", target_os = "windows")) { name.eq_ignore_ascii_case(wanted) } else { name == wanted }
}

fn named_with_prefix(name: &str, prefix: &str) -> bool {
    name.get(..prefix.len()).is_some_and(|start| named(start, prefix))
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
        let after = |folder: &str| previous.as_deref().is_some_and(|p| named(p, folder));
        let provider = if after("Mobile Documents") && named(&name, "com~apple~CloudDocs") {
            Some(Provider::ICloudDrive)
        } else if named(&name, "iCloudDrive") {
            // iCloud for Windows keeps the folder in the user's profile.
            Some(Provider::ICloudDrive)
        } else if after("CloudStorage") && named_with_prefix(&name, "GoogleDrive-") {
            Some(Provider::GoogleDrive)
        } else if after("CloudStorage") && (named_with_prefix(&name, "OneDrive-") || named(&name, "OneDrive")) {
            Some(Provider::OneDrive)
        } else {
            None
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
