//! UI-independent part of the built-in file browser: listing a folder and
//! the places to start from. Used where the system's "Open file" dialog
//! cannot be shown (Steam Deck's Game Mode).

use std::io;
use std::path::{Path, PathBuf};

/// One row of a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// The folders and files in `dir`, folders first, each sorted by name
/// (ignoring case). Hidden entries (`.name`) are left out. Files are kept
/// only if their extension is in `extensions` (ignoring case), unless
/// `extensions` is empty.
pub fn list_dir(dir: &Path, extensions: &[&str]) -> io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = e.path();
            // `is_dir` follows symlinks, so linked folders open too.
            let is_dir = path.is_dir();
            if !is_dir && !has_extension(&name, extensions) {
                return None;
            }
            Some(Entry { name, path, is_dir })
        })
        .collect();
    entries.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase()));
    Ok(entries)
}

/// Whether `name` ends in one of `extensions` (no dot, any case); an empty
/// list matches everything.
pub fn has_extension(name: &str, extensions: &[&str]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// Named folders to jump to: the home folder and those of its usual
/// subfolders that exist.
pub fn places(home: Option<&Path>) -> Vec<(String, PathBuf)> {
    let Some(home) = home else {
        return Vec::new();
    };
    let mut out = vec![("Home".to_string(), home.to_path_buf())];
    for name in ["Downloads", "Documents", "Desktop"] {
        let dir = home.join(name);
        if dir.is_dir() {
            out.push((name.to_string(), dir));
        }
    }
    out
}

/// The home folder from the environment (`HOME`, or `USERPROFILE` on
/// Windows).
pub fn home_dir(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env("HOME")
        .or_else(|| env("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The folder to show first: `preferred` if it exists, else Downloads,
/// else home, else the current directory.
pub fn first_dir(preferred: Option<PathBuf>, home: Option<&Path>) -> PathBuf {
    if let Some(dir) = preferred.filter(|d| d.is_dir()) {
        return dir;
    }
    if let Some(home) = home {
        let downloads = home.join("Downloads");
        if downloads.is_dir() {
            return downloads;
        }
        if home.is_dir() {
            return home.to_path_buf();
        }
    }
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openzap-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extensions_match_ignoring_case() {
        let exts = ["m3u", "m3u8"];
        assert!(has_extension("list.m3u", &exts));
        assert!(has_extension("playlist (2).M3U8", &exts));
        assert!(!has_extension("guide.xml", &exts));
        assert!(!has_extension("m3u", &exts));
        assert!(has_extension("anything", &[]));
    }

    #[test]
    fn folders_come_first_and_other_files_are_hidden() {
        let dir = temp_dir("list");
        std::fs::create_dir(dir.join("b-folder")).unwrap();
        std::fs::create_dir(dir.join("A-folder")).unwrap();
        std::fs::create_dir(dir.join(".hidden")).unwrap();
        for f in ["z.m3u", "B.M3U8", "notes.txt", ".secret.m3u"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let names = |exts: &[&str]| -> Vec<String> {
            list_dir(&dir, exts)
                .unwrap()
                .into_iter()
                .map(|e| e.name)
                .collect()
        };
        assert_eq!(
            names(&["m3u", "m3u8"]),
            ["A-folder", "b-folder", "B.M3U8", "z.m3u"]
        );
        assert_eq!(
            names(&[]),
            ["A-folder", "b-folder", "B.M3U8", "notes.txt", "z.m3u"]
        );
        let first = &list_dir(&dir, &[]).unwrap()[0];
        assert!(first.is_dir && first.path == dir.join("A-folder"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_folder_is_an_error() {
        assert!(list_dir(Path::new("/no/such/folder"), &[]).is_err());
    }

    #[test]
    fn places_list_existing_folders_only() {
        let home = temp_dir("places");
        std::fs::create_dir(home.join("Downloads")).unwrap();
        let names: Vec<String> = places(Some(&home)).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["Home", "Downloads"]);
        assert!(places(None).is_empty());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn home_comes_from_the_environment() {
        let env = |name: &str| (name == "HOME").then(|| "/home/deck".to_string());
        assert_eq!(home_dir(env), Some(PathBuf::from("/home/deck")));
        let windows = |name: &str| (name == "USERPROFILE").then(|| r"C:\Users\a".to_string());
        assert_eq!(home_dir(windows), Some(PathBuf::from(r"C:\Users\a")));
        assert_eq!(home_dir(|_| Some(String::new())), None);
    }

    #[test]
    fn first_dir_prefers_the_given_then_downloads() {
        let home = temp_dir("first");
        let given = home.join("lists");
        std::fs::create_dir(&given).unwrap();
        assert_eq!(first_dir(Some(given.clone()), Some(&home)), given);
        assert_eq!(first_dir(None, Some(&home)), home);
        std::fs::create_dir(home.join("Downloads")).unwrap();
        assert_eq!(
            first_dir(Some(home.join("gone")), Some(&home)),
            home.join("Downloads")
        );
        assert_eq!(first_dir(None, None), PathBuf::from("."));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
