//! The app's cache folder (logos, saved copies of playlists and guides).

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::config::Platform;

/// The cache folder, given an environment lookup.
pub fn root(platform: Platform, env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let non_empty = |name: &str| env(name).filter(|v| !v.is_empty());
    Some(match platform {
        Platform::Windows => PathBuf::from(non_empty("LOCALAPPDATA")?)
            .join("OpenZap")
            .join("cache"),
        Platform::MacOs => PathBuf::from(non_empty("HOME")?)
            .join("Library")
            .join("Caches")
            .join("OpenZap"),
        Platform::Unix => match non_empty("XDG_CACHE_HOME") {
            Some(dir) => PathBuf::from(dir).join("openzap"),
            None => PathBuf::from(non_empty("HOME")?)
                .join(".cache")
                .join("openzap"),
        },
    })
}

/// The cache folder for the current platform and environment.
pub fn default_root() -> Option<PathBuf> {
    root(Platform::current(), |name| std::env::var(name).ok())
}

/// A file name for `key` (e.g. a URL) that is safe on every platform and
/// does not reveal the key: its FNV-1a hash plus `ext`.
pub fn file_name(key: &str, ext: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}.{ext}")
}

/// Creates (or truncates) a file only the owner can read on Unix: saved
/// playlists and settings may contain access keys.
pub fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &'static [(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        move |name: &str| map.get(name).map(|v| v.to_string())
    }

    #[test]
    fn roots_per_platform() {
        assert_eq!(
            root(Platform::Unix, env(&[("HOME", "/home/me")])),
            Some(PathBuf::from("/home/me/.cache/openzap"))
        );
        assert_eq!(
            root(
                Platform::Unix,
                env(&[("HOME", "/h"), ("XDG_CACHE_HOME", "/c")])
            ),
            Some(PathBuf::from("/c/openzap"))
        );
        assert_eq!(
            root(Platform::Windows, env(&[("LOCALAPPDATA", "C:\\L")])),
            Some(PathBuf::from("C:\\L").join("OpenZap/cache"))
        );
        assert_eq!(
            root(Platform::MacOs, env(&[("HOME", "/Users/me")])),
            Some(PathBuf::from("/Users/me/Library/Caches/OpenZap"))
        );
        assert_eq!(root(Platform::Unix, env(&[])), None);
        assert_eq!(root(Platform::Windows, env(&[("LOCALAPPDATA", "")])), None);
    }

    #[test]
    fn file_names_are_hashes() {
        assert_eq!(file_name("", "img"), "cbf29ce484222325.img");
        let a = file_name("http://example.com/a?token=fakekey", "epg");
        assert!(!a.contains("fakekey") && a.ends_with(".epg"));
        assert_ne!(a, file_name("http://example.com/b?token=fakekey", "epg"));
    }

    #[test]
    fn private_files_are_owner_only() {
        let dir = std::env::temp_dir().join(format!("openzap-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x");
        create_private(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
