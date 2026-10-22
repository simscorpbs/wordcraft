//! What a version records about how and where it was saved, without tracking a person.
//!
//! Only one thing goes into the document itself: a random document id ([`DOC_ID_PROP`], a
//! UUID v4 that derives from nothing: not the machine, the user, the time or the path). The
//! history's own records stay in the history folder and hold counts and hashes, never text of
//! the document. The computer appears as a random installation id and a label the user can
//! change ("Windows", "Mac" by default), never its host name or the account name; paths are
//! kept relative to the user's folder or the cloud provider's folder.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// The custom document property (ECMA-376 Part 1 §22.3, `docProps/custom.xml`) that holds the
/// document id. Word keeps custom properties when it saves (they are the Custom tab of
/// Advanced Properties), so the id survives a round trip through Word.
pub(crate) const DOC_ID_PROP: &str = "WordCraftDocumentId";
/// The installation id's file in the settings folder.
const INSTALLATION_FILE: &str = "installation-id";
/// Largest `docProps/custom.xml` looked at for the id.
const MAX_CUSTOM_XML: u64 = 1 << 20;
/// Most distinct words kept to count what an edit inserted and removed.
const MAX_BAG_WORDS: usize = 200_000;

fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A new document id: a random UUID (version 4, RFC 9562 §5.4), lower-case.
pub(crate) fn new_document_id() -> Result<String, String> {
    let mut b = random_bytes::<16>()?;
    if let Some(byte) = b.get_mut(6) {
        *byte = (*byte & 0x0f) | 0x40;
    }
    if let Some(byte) = b.get_mut(8) {
        *byte = (*byte & 0x3f) | 0x80;
    }
    let h = hex(&b);
    let part = |range: std::ops::Range<usize>| h.get(range).unwrap_or_default();
    Ok(format!("{}-{}-{}-{}-{}", part(0..8), part(8..12), part(12..16), part(16..20), part(20..32)))
}

/// Is `text` a document id as WordCraft writes them (a UUID in its usual text form)? Ids come
/// from files, so anything else is ignored.
pub(crate) fn document_id_ok(text: &str) -> bool {
    text.len() == 36
        && text
            .bytes()
            .enumerate()
            .all(|(i, b)| if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() && !b.is_ascii_uppercase() })
}

/// The history folder of a document id: its 32 hex digits.
pub(crate) fn id_bucket(id: &str) -> String {
    id.chars().filter(|c| *c != '-').collect()
}

/// The document id in a Word package, read from `docProps/custom.xml` without opening the
/// document. `None` for anything that is not a package with a valid id.
pub(crate) fn package_document_id<R: Read + std::io::Seek>(reader: R) -> Option<String> {
    let mut archive = zip::ZipArchive::new(reader).ok()?;
    let mut part = archive.by_name("docProps/custom.xml").ok()?;
    if part.size() > MAX_CUSTOM_XML {
        return None;
    }
    let mut xml = String::new();
    part.by_ref().take(MAX_CUSTOM_XML).read_to_string(&mut xml).ok()?;
    let at = xml.find(&format!("name=\"{DOC_ID_PROP}\"")).or_else(|| xml.find(&format!("name='{DOC_ID_PROP}'")))?;
    let rest = xml.get(at..)?;
    let end = rest.find("</property>").unwrap_or(rest.len());
    let property = rest.get(..end)?;
    let value_start = property.find("lpwstr>")? + "lpwstr>".len();
    let value = property.get(value_start..)?.split('<').next()?.trim();
    document_id_ok(value).then(|| value.to_owned())
}

/// The document id of the Word package at `path` (only its directory and that part are read).
pub(crate) fn file_document_id(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    package_document_id(std::io::BufReader::new(file))
}

/// This installation's random id: made once and kept in the settings folder. It says which
/// computer saved a version without saying anything about the computer.
pub(crate) fn installation_id(settings: &Path) -> String {
    let path = settings.join(INSTALLATION_FILE);
    if let Ok(meta) = std::fs::symlink_metadata(&path)
        && meta.is_file()
        && meta.len() <= 64
        && let Ok(text) = std::fs::read_to_string(&path)
    {
        let id = text.trim();
        if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return id.to_ascii_lowercase();
        }
    }
    let id = random_bytes::<16>().map(|b| hex(&b)).unwrap_or_default();
    // Not kept (a read-only settings folder): versions of this run still share one id.
    let _ = super::write_private(&path, id.as_bytes());
    id
}

/// The operating system's version number, looked up once per run (never its name for the
/// machine).
pub(crate) fn os_version() -> String {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(read_os_version).clone()
}

fn version_digits(text: &str) -> String {
    let start = text.find(|c: char| c.is_ascii_digit()).unwrap_or(text.len());
    text.get(start..).unwrap_or("").chars().take_while(|c| c.is_ascii_digit() || *c == '.').take(32).collect()
}

fn read_os_version() -> String {
    #[cfg(windows)]
    {
        // `ver` prints "Microsoft Windows [Version 10.0.26100.1]".
        let output = std::process::Command::new("cmd").args(["/d", "/c", "ver"]).output();
        output
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|t| t.rsplit_once(' ').map(|(_, v)| version_digits(v)).unwrap_or_default())
            .unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sw_vers").arg("-productVersion").output();
        output.ok().and_then(|o| String::from_utf8(o.stdout).ok()).map(|t| version_digits(&t)).unwrap_or_default()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        text.lines().find_map(|line| line.strip_prefix("VERSION_ID=")).map(|v| version_digits(v.trim_matches('"'))).unwrap_or_default()
    }
}

/// The account name, only so it can be kept out of what is recorded.
fn account_name() -> Option<String> {
    ["USERNAME", "USER", "LOGNAME"].iter().find_map(|k| std::env::var(k).ok()).filter(|n| !n.trim().is_empty())
}

fn home_dir() -> Option<PathBuf> {
    ["USERPROFILE", "HOME"].iter().find_map(std::env::var_os).filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// `path` without Windows' `\\?\` prefix.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    text.strip_prefix(r"\\?\").map_or_else(|| path.to_path_buf(), PathBuf::from)
}

/// `path` below `base`, compared without case on Windows (where file names ignore it).
fn below(path: &Path, base: &Path) -> Option<PathBuf> {
    let (path, base) = (plain(path), plain(base));
    if let Ok(rest) = path.strip_prefix(&base) {
        return Some(rest.to_path_buf());
    }
    if cfg!(windows) {
        let mut parts = path.components();
        for want in base.components() {
            let got = parts.next()?;
            if got.as_os_str().to_string_lossy().to_lowercase() != want.as_os_str().to_string_lossy().to_lowercase() {
                return None;
            }
        }
        return Some(parts.as_path().to_path_buf());
    }
    None
}

fn slashed(rest: &Path) -> String {
    rest.components()
        .filter_map(|c| if let Component::Normal(n) = c { Some(n.to_string_lossy().into_owned()) } else { None })
        .collect::<Vec<_>>()
        .join("/")
}

/// The provider a document is kept with: `local`, `icloud`, `onedrive` or `gdrive`.
pub(crate) fn provider_of(path: &Path, roots: &[wordcraft_cloud::CloudRoot]) -> &'static str {
    wordcraft_cloud::locate(path, roots).map_or("local", |l| l.provider.id())
}

/// The path a version records: below the provider's folder (`OneDrive:/Reports/x.docx`) or the
/// user's folder (`~/Documents/x.docx`), and any folder named like the account replaced by
/// `<user>`, so the account name is never kept.
pub(crate) fn portable_path(path: &Path, roots: &[wordcraft_cloud::CloudRoot]) -> String {
    let path = plain(path);
    let text = if let Some(location) = wordcraft_cloud::locate(&path, roots)
        && let Some(rest) = below(&path, &location.root)
    {
        format!("{}:/{}", location.provider.id(), slashed(&rest))
    } else if let Some(home) = home_dir()
        && let Some(rest) = below(&path, &home)
    {
        format!("~/{}", slashed(&rest))
    } else {
        path.to_string_lossy().replace('\\', "/")
    };
    match account_name() {
        Some(account) => text
            .split('/')
            .map(|part| if part.eq_ignore_ascii_case(&account) || part.eq_ignore_ascii_case(&format!("{account}.local")) { "<user>" } else { part })
            .collect::<Vec<_>>()
            .join("/"),
        None => text,
    }
}

/// A recorded path back on this computer, when it can be (not when a part was hidden).
pub(crate) fn resolve_portable_path(text: &str, roots: &[wordcraft_cloud::CloudRoot]) -> Option<PathBuf> {
    if text.split('/').any(|part| part == "<user>" || part == "..") {
        return None;
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return home_dir().map(|home| home.join(rest));
    }
    if let Some((provider, rest)) = text.split_once(":/")
        && let Some(provider) = wordcraft_cloud::Provider::from_id(provider)
    {
        return roots.iter().find(|r| r.provider == provider).map(|r| r.path.join(rest));
    }
    let path = PathBuf::from(text);
    path.is_absolute().then_some(path)
}

/// How often each word occurs: kept in memory only, to count what the next save inserted and
/// removed. Never written anywhere.
pub(crate) type WordBag = HashMap<String, u32>;

pub(crate) fn word_bag(text: &str) -> WordBag {
    let mut bag = WordBag::new();
    for word in text.split_whitespace() {
        if bag.len() >= MAX_BAG_WORDS && !bag.contains_key(word) {
            continue;
        }
        *bag.entry(word.to_owned()).or_insert(0) += 1;
    }
    bag
}

/// Words inserted and removed going from `old` to `new`, counted by word (an approximation:
/// a moved word counts as neither).
pub(crate) fn bag_diff(old: &WordBag, new: &WordBag) -> (u64, u64) {
    let inserted = new.iter().map(|(w, n)| u64::from(n.saturating_sub(old.get(w).copied().unwrap_or(0)))).sum();
    let removed = old.iter().map(|(w, n)| u64::from(n.saturating_sub(new.get(w).copied().unwrap_or(0)))).sum();
    (inserted, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_ids_are_random_uuids() {
        let a = new_document_id().unwrap();
        let b = new_document_id().unwrap();
        assert_ne!(a, b);
        assert!(document_id_ok(&a), "{a}");
        assert_eq!(a.as_bytes()[14], b'4', "version 4");
        assert!(matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'), "RFC variant");
        assert_eq!(id_bucket(&a).len(), 32);
        for bad in [
            "",
            "../../etc/passwd",
            "ZZZZZZZZ-ZZZZ-4ZZZ-8ZZZ-ZZZZZZZZZZZZ",
            "12345678-1234-4234-8234-1234567890AB",
            "12345678123441234823412345678901ab",
        ] {
            assert!(!document_id_ok(bad), "{bad}");
        }
    }

    #[test]
    fn word_counts_of_an_edit() {
        let old = word_bag("the quick brown fox jumps");
        let new = word_bag("the quick red fox jumps high");
        assert_eq!(bag_diff(&old, &new), (2, 1));
    }

    #[test]
    fn recorded_paths_leave_out_the_account() {
        let home = home_dir().unwrap();
        let file = home.join("Documents").join("Report.docx");
        assert_eq!(portable_path(&file, &[]), "~/Documents/Report.docx");
        assert_eq!(resolve_portable_path("~/Documents/Report.docx", &[]), Some(home.join("Documents/Report.docx")));
        let root = wordcraft_cloud::CloudRoot { provider: wordcraft_cloud::Provider::OneDrive, path: home.join("OneDrive"), account: None };
        let cloud = home.join("OneDrive").join("Team").join("Plan.docx");
        assert_eq!(portable_path(&cloud, std::slice::from_ref(&root)), "onedrive:/Team/Plan.docx");
        assert_eq!(
            resolve_portable_path("onedrive:/Team/Plan.docx", std::slice::from_ref(&root)),
            Some(home.join("OneDrive").join("Team/Plan.docx"))
        );
        if let Some(account) = account_name() {
            let elsewhere = PathBuf::from(if cfg!(windows) { r"D:\Shared" } else { "/srv/shared" }).join(&account).join("x.docx");
            let text = portable_path(&elsewhere, &[]);
            assert!(!text.to_lowercase().contains(&account.to_lowercase()), "{text}");
            assert!(resolve_portable_path(&text, &[]).is_none());
        }
    }
}
