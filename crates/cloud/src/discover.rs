//! Finding the folders the providers' sync apps keep on this computer.
//!
//! Where each app keeps its folder, by system:
//!
//! | | macOS | Windows |
//! |---|---|---|
//! | iCloud Drive | `~/Library/Mobile Documents/com~apple~CloudDocs` | `%USERPROFILE%\iCloudDrive` |
//! | Google Drive | `~/Library/CloudStorage/GoogleDrive-<account>` (older: `~/Google Drive`) | a drive letter holding `My Drive` (streaming), or `%USERPROFILE%\My Drive` (mirroring) |
//! | OneDrive | `~/Library/CloudStorage/OneDrive-<account>` (older: `~/OneDrive`) | `%OneDriveConsumer%`, `%OneDriveCommercial%`, `%OneDrive%` |
//!
//! `WORDCRAFT_CLOUD_ICLOUD`, `WORDCRAFT_CLOUD_GDRIVE` and `WORDCRAFT_CLOUD_ONEDRIVE` name a
//! folder for a provider on any system (several separated as `PATH` is), for setups the table
//! misses (a Linux sync client, a moved folder) and for tests. Discovery only looks at folder
//! names; it reads no file and needs no account.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::{CloudRoot, Provider};

/// Google Drive's "My Drive" folder as Drive for desktop names it in the languages it ships in
/// most often.
pub(crate) const MY_DRIVE_NAMES: &[&str] = &["My Drive", "Meu Drive", "Mi unidad", "Mon Drive", "Meine Ablage", "Il mio Drive", "Mijn Drive"];

/// The system whose conventions discovery follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Windows,
    /// Linux, BSD and the rest: no official sync apps, only the `WORDCRAFT_CLOUD_*` folders.
    Other,
}

/// What discovery looks at: the system, the home folder and the environment. [`Env::current`]
/// is this computer; tests build their own.
#[derive(Clone, Debug)]
pub struct Env {
    pub os: Os,
    pub home: Option<PathBuf>,
    pub vars: BTreeMap<String, OsString>,
    /// Drive roots to look for Google Drive's streaming drive in (Windows): `D:\` to `Z:\`.
    pub drives: Vec<PathBuf>,
}

impl Env {
    /// This computer.
    pub fn current() -> Env {
        let os = if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "windows") {
            Os::Windows
        } else {
            Os::Other
        };
        let wanted = ["HOME", "USERPROFILE", "OneDrive", "OneDriveConsumer", "OneDriveCommercial"];
        let vars = std::env::vars_os()
            .filter_map(|(k, v)| {
                let k = k.into_string().ok()?;
                (wanted.contains(&k.as_str()) || k.starts_with("WORDCRAFT_CLOUD_")).then_some((k, v))
            })
            .collect::<BTreeMap<_, _>>();
        let home_var = if os == Os::Windows { "USERPROFILE" } else { "HOME" };
        let home = vars.get(home_var).filter(|h| !h.is_empty()).map(PathBuf::from);
        let drives = if os == Os::Windows { ('D'..='Z').map(|letter| PathBuf::from(format!("{letter}:\\"))).collect() } else { Vec::new() };
        Env { os, home, vars, drives }
    }

    fn var(&self, name: &str) -> Option<PathBuf> {
        self.vars.get(name).filter(|v| !v.is_empty()).map(PathBuf::from)
    }
}

/// The providers' folders on this computer, iCloud Drive first, then Google Drive and OneDrive;
/// each folder once. None in the browser build.
pub fn discover(env: &Env) -> Vec<CloudRoot> {
    if cfg!(target_arch = "wasm32") {
        return Vec::new();
    }
    let mut found = Vec::new();
    for provider in Provider::ALL {
        let var = format!("WORDCRAFT_CLOUD_{}", provider.id().to_ascii_uppercase());
        if let Some(list) = env.vars.get(&var) {
            for path in std::env::split_paths(list).filter(|p| p.is_dir()) {
                found.push(CloudRoot { provider, path, account: None });
            }
        }
    }
    match env.os {
        Os::MacOs => mac(env, &mut found),
        Os::Windows => windows(env, &mut found),
        Os::Other => {}
    }
    let mut roots: Vec<CloudRoot> = Vec::new();
    for root in found {
        if !roots.iter().any(|r| r.path == root.path) {
            roots.push(root);
        }
    }
    // Stable: within a provider, the order found (overrides first).
    roots.sort_by_key(|r| r.provider);
    roots
}

fn mac(env: &Env, found: &mut Vec<CloudRoot>) {
    let Some(home) = &env.home else { return };
    push_dir(found, Provider::ICloudDrive, home.join("Library").join("Mobile Documents").join("com~apple~CloudDocs"), None);
    // File Provider folders (Google Drive for desktop, OneDrive), one per signed-in account.
    let mut storage: Vec<(String, PathBuf)> = std::fs::read_dir(home.join("Library").join("CloudStorage"))
        .map(|entries| entries.filter_map(Result::ok).map(|e| (e.file_name().to_string_lossy().into_owned(), e.path())).collect())
        .unwrap_or_default();
    storage.sort();
    for (name, path) in storage {
        if let Some(account) = name.strip_prefix("GoogleDrive-") {
            push_dir(found, Provider::GoogleDrive, path, Some(account.to_string()));
        } else if let Some(account) = name.strip_prefix("OneDrive-") {
            push_dir(found, Provider::OneDrive, path, Some(account.to_string()));
        }
    }
    // Before File Provider: folders in the home folder.
    push_dir(found, Provider::GoogleDrive, home.join("Google Drive"), None);
    push_dir(found, Provider::OneDrive, home.join("OneDrive"), None);
}

fn windows(env: &Env, found: &mut Vec<CloudRoot>) {
    if let Some(home) = &env.home {
        push_dir(found, Provider::ICloudDrive, home.join("iCloudDrive"), None);
    }
    // Drive for desktop streams to a drive letter whose root holds My Drive, or mirrors into the
    // profile folder.
    for drive in &env.drives {
        if MY_DRIVE_NAMES.iter().any(|name| drive.join(name).is_dir()) {
            push_dir(found, Provider::GoogleDrive, drive.clone(), None);
        }
    }
    if let Some(home) = &env.home {
        for name in MY_DRIVE_NAMES {
            push_dir(found, Provider::GoogleDrive, home.join(name), None);
        }
        push_dir(found, Provider::GoogleDrive, home.join("Google Drive"), None);
    }
    for var in ["OneDriveConsumer", "OneDriveCommercial", "OneDrive"] {
        if let Some(path) = env.var(var) {
            let account = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("OneDrive - ")).map(str::to_string);
            push_dir(found, Provider::OneDrive, path, account);
        }
    }
}

fn push_dir(found: &mut Vec<CloudRoot>, provider: Provider, path: PathBuf, account: Option<String>) {
    if path.is_dir() {
        found.push(CloudRoot { provider, path, account });
    }
}
