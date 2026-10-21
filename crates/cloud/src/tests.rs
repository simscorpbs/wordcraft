use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;

/// A fresh, empty folder for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wc-cloud-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn env(os: Os, home: &Path) -> Env {
    Env { os, home: Some(home.to_path_buf()), vars: BTreeMap::new(), drives: Vec::new() }
}

#[test]
fn finds_the_mac_sync_folders_in_priority_order() {
    let home = scratch("mac");
    let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
    let google = home.join("Library/CloudStorage/GoogleDrive-ana@example.com");
    let onedrive = home.join("Library/CloudStorage/OneDrive-Personal");
    for dir in [&onedrive, &google, &icloud] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // Other File Provider folders are someone else's (Dropbox, Box…).
    std::fs::create_dir_all(home.join("Library/CloudStorage/Dropbox")).unwrap();
    let roots = discover(&env(Os::MacOs, &home));
    let got: Vec<(Provider, &Path, Option<&str>)> = roots.iter().map(|r| (r.provider, r.path.as_path(), r.account.as_deref())).collect();
    assert_eq!(
        got,
        [
            (Provider::ICloudDrive, icloud.as_path(), None),
            (Provider::GoogleDrive, google.as_path(), Some("ana@example.com")),
            (Provider::OneDrive, onedrive.as_path(), Some("Personal")),
        ]
    );
    // The same folders don't count on another system.
    assert!(discover(&env(Os::Other, &home)).is_empty());
}

#[test]
fn finds_the_windows_sync_folders() {
    let home = scratch("win");
    let icloud = home.join("iCloudDrive");
    let drive = home.join("G");
    let business = home.join("OneDrive - Contoso");
    std::fs::create_dir_all(&icloud).unwrap();
    std::fs::create_dir_all(drive.join("Meu Drive")).unwrap();
    std::fs::create_dir_all(&business).unwrap();
    let mut e = env(Os::Windows, &home);
    e.drives = vec![drive.clone(), home.join("missing-drive")];
    e.vars.insert("OneDriveCommercial".into(), business.clone().into_os_string());
    // %OneDrive% names the same folder again: it is listed once.
    e.vars.insert("OneDrive".into(), business.clone().into_os_string());
    let roots = discover(&e);
    let got: Vec<(Provider, &Path, Option<&str>)> = roots.iter().map(|r| (r.provider, r.path.as_path(), r.account.as_deref())).collect();
    assert_eq!(
        got,
        [
            (Provider::ICloudDrive, icloud.as_path(), None),
            (Provider::GoogleDrive, drive.as_path(), None),
            (Provider::OneDrive, business.as_path(), Some("Contoso"))
        ]
    );
    // A new Google Drive document goes in My Drive, by its local name.
    assert_eq!(roots[1].save_dir(), drive.join("Meu Drive"));
}

#[test]
fn environment_folders_count_on_any_system() {
    let home = scratch("override");
    let a = home.join("rclone-gdrive");
    std::fs::create_dir_all(&a).unwrap();
    let mut e = env(Os::Other, &home);
    e.vars.insert("WORDCRAFT_CLOUD_GDRIVE".into(), std::env::join_paths([a.clone(), home.join("not-there")]).unwrap());
    let roots = discover(&e);
    assert_eq!(roots, [CloudRoot { provider: Provider::GoogleDrive, path: a, account: None }]);
}

#[test]
fn locates_documents_by_root_and_by_folder_names() {
    let roots = [
        CloudRoot { provider: Provider::OneDrive, path: PathBuf::from("/Users/ana/OneDrive"), account: None },
        CloudRoot { provider: Provider::GoogleDrive, path: PathBuf::from("/Users/ana/OneDrive/Synced/Drive"), account: None },
    ];
    let at = |p: &str| locate(Path::new(p), &roots).map(|l| (l.provider, l.root));
    assert_eq!(at("/Users/ana/OneDrive/a.docx"), Some((Provider::OneDrive, PathBuf::from("/Users/ana/OneDrive"))));
    // The innermost folder wins.
    assert_eq!(at("/Users/ana/OneDrive/Synced/Drive/b.docx"), Some((Provider::GoogleDrive, PathBuf::from("/Users/ana/OneDrive/Synced/Drive"))));
    // Whole components only.
    assert_eq!(at("/Users/ana/OneDrive2/a.docx"), None);
    assert_eq!(at("/Users/ana/OneDrive"), None);
    // Folder names say where a document is even without discovery.
    assert_eq!(
        at("/Users/bo/Library/Mobile Documents/com~apple~CloudDocs/Notes/c.docx"),
        Some((Provider::ICloudDrive, PathBuf::from("/Users/bo/Library/Mobile Documents/com~apple~CloudDocs")))
    );
    assert_eq!(
        at("/Users/bo/Library/CloudStorage/GoogleDrive-bo@example.com/My Drive/d.docx"),
        Some((Provider::GoogleDrive, PathBuf::from("/Users/bo/Library/CloudStorage/GoogleDrive-bo@example.com")))
    );
    assert_eq!(at("/Users/bo/Library/CloudStorage/OneDrive-Personal/e.docx").map(|l| l.0), Some(Provider::OneDrive));
    assert_eq!(at("/Users/bo/Documents/f.docx"), None);
    // A document merely named like a provider's folder isn't in it.
    assert_eq!(at("/Users/bo/Library/Mobile Documents/com~apple~CloudDocs"), None);
}

/// Windows spells one folder several ways: a verbatim `\\?\` prefix (what `canonicalize` returns),
/// another case, `/` for `\`. All of them are inside the discovered root (AutoSave was blocked for
/// `\\?\C:\Users\ana\OneDrive\a.docx`).
#[cfg(windows)]
#[test]
fn windows_spellings_of_a_root_are_the_same_folder() {
    let roots = [
        CloudRoot { provider: Provider::OneDrive, path: PathBuf::from(r"C:\Users\ana\OneDrive"), account: None },
        CloudRoot { provider: Provider::GoogleDrive, path: PathBuf::from(r"\\nas\Drive"), account: None },
    ];
    let at = |p: &str| locate(Path::new(p), &roots).map(|l| l.provider);
    for p in [r"\\?\C:\Users\ana\OneDrive\a.docx", r"c:\users\ANA\onedrive\a.docx", "C:/Users/ana/OneDrive/Sub/a.docx"] {
        assert_eq!(at(p), Some(Provider::OneDrive), "{p}");
    }
    for p in [r"\\?\UNC\nas\Drive\b.docx", r"\\NAS\drive\b.docx"] {
        assert_eq!(at(p), Some(Provider::GoogleDrive), "{p}");
    }
    let verbatim_root = [CloudRoot { provider: Provider::OneDrive, path: PathBuf::from(r"\\?\C:\Users\ana\OneDrive"), account: None }];
    assert_eq!(locate(Path::new(r"C:\Users\ana\OneDrive\a.docx"), &verbatim_root).map(|l| l.provider), Some(Provider::OneDrive));
    // Another drive, or a device path, is not the same folder.
    assert_eq!(at(r"\\?\D:\Users\ana\OneDrive\a.docx"), None);
    assert_eq!(at(r"\\.\C:\Users\ana\OneDrive\a.docx"), None);
    // Folder names count without regard to case too.
    assert_eq!(locate(Path::new(r"\\?\C:\Users\bo\ICLOUDDRIVE\c.docx"), &[]).map(|l| l.provider), Some(Provider::ICloudDrive));
}

/// A document reached through short 8.3 folder names is still inside its root (it opened with
/// AutoSave blocked: `C:\…\ONEDRI~1\DOCUME~1\Plan.docx` matched no discovered folder).
#[cfg(windows)]
#[test]
fn short_names_are_inside_the_root_with_the_long_name() {
    let dir = scratch("short-names");
    let root = std::fs::canonicalize(&dir).unwrap().join("OneDrive Pessoal");
    let docs = root.join("Documentos Longos");
    std::fs::create_dir_all(&docs).unwrap();
    let doc = docs.join("Plano Anual.docx");
    std::fs::write(&doc, b"x").unwrap();
    let out = std::process::Command::new("cmd").args(["/C", &format!("for %I in (\"{}\") do @echo %~sI", doc.display())]).output().unwrap();
    let short = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let plain_root = PathBuf::from(root.to_string_lossy().trim_start_matches(r"\\?\"));
    let roots = [CloudRoot { provider: Provider::OneDrive, path: plain_root.clone(), account: None }];
    if short.to_string_lossy().contains('~') {
        assert_eq!(locate(&short, &roots), Some(CloudLocation { provider: Provider::OneDrive, root: plain_root }), "{}", short.display());
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn provider_ids_round_trip() {
    for p in Provider::ALL {
        assert_eq!(Provider::from_id(p.id()), Some(p));
        assert_eq!(Provider::from_id(p.name()), Some(p));
    }
    assert_eq!(Provider::from_id("dropbox"), None);
}

#[test]
fn stamps_tell_a_real_change_from_a_touched_date() {
    let dir = scratch("stamp");
    let path = dir.join("a.docx");
    std::fs::write(&path, b"version one").unwrap();
    let mut stamp = DiskStamp::read(&path).unwrap();
    assert_eq!(stamp.check(&path).unwrap(), DiskState::Unchanged);

    // A sync app rewrites the same bytes with a new date: still unchanged.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
    assert_eq!(stamp.check(&path).unwrap(), DiskState::Unchanged);
    assert_eq!(stamp.check(&path).unwrap(), DiskState::Unchanged);

    // Another device saved: same size, different bytes.
    std::fs::write(&path, b"version two").unwrap();
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(later + std::time::Duration::from_secs(60)).unwrap();
    assert_eq!(stamp.check(&path).unwrap(), DiskState::Changed);
    std::fs::write(&path, b"a longer version three").unwrap();
    assert_eq!(stamp.check(&path).unwrap(), DiskState::Changed);

    // Our own save stamps the bytes it wrote.
    std::fs::write(&path, b"mine").unwrap();
    let mut stamp = DiskStamp::of_bytes(&path, b"mine").unwrap();
    assert_eq!(stamp.check(&path).unwrap(), DiskState::Unchanged);

    std::fs::remove_file(&path).unwrap();
    assert_eq!(stamp.check(&path).unwrap(), DiskState::Missing);
    // A folder where the file was is no file.
    std::fs::create_dir(&path).unwrap();
    assert_eq!(stamp.check(&path).unwrap(), DiskState::Missing);
}

#[test]
fn new_and_conflict_names_never_take_an_existing_file() {
    let dir = scratch("names");
    assert_eq!(unique_path(&dir, "Report", "docx").unwrap(), dir.join("Report.docx"));
    std::fs::write(dir.join("Report.docx"), b"").unwrap();
    assert_eq!(unique_path(&dir, "Report", ".docx").unwrap(), dir.join("Report 2.docx"));
    // iCloud's placeholder for a cloud-only "Report 2.docx" takes that name too.
    std::fs::write(dir.join(".Report 2.docx.icloud"), b"").unwrap();
    assert_eq!(unique_path(&dir, "Report", "docx").unwrap(), dir.join("Report 3.docx"));
    // Names that can't be file names are cleaned.
    assert_eq!(unique_path(&dir, "a/b:c", "docx").unwrap(), dir.join("a-b-c.docx"));
    assert_eq!(unique_path(&dir, " .. ", "docx").unwrap(), dir.join("Document.docx"));
    assert_eq!(unique_path(&dir, &"x".repeat(500), "docx").unwrap().file_name().unwrap().len(), 205);

    let original = dir.join("Report.docx");
    let copy = conflict_copy_path(&original, "conflict copy").unwrap();
    assert_eq!(copy, dir.join("Report (conflict copy).docx"));
    std::fs::write(&copy, b"").unwrap();
    assert_eq!(conflict_copy_path(&original, "conflict copy").unwrap(), dir.join("Report (conflict copy) 2.docx"));
}

#[test]
fn icloud_placeholders_are_found_only_for_missing_files() {
    let dir = scratch("placeholder");
    let path = dir.join("Thesis.docx");
    assert_eq!(icloud_placeholder(&path), None);
    std::fs::write(dir.join(".Thesis.docx.icloud"), b"").unwrap();
    assert_eq!(icloud_placeholder(&path), Some(dir.join(".Thesis.docx.icloud")));
    std::fs::write(&path, b"downloaded").unwrap();
    assert_eq!(icloud_placeholder(&path), None);
}
