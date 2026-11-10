//! The app's cache folder (logos, saved copies of playlists and guides).

use std::fs::File;
use std::io;
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

/// Total size of the files under `dir`; 0 if it does not exist.
pub fn size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => size(&entry.path()),
            Ok(t) if t.is_file() => entry.metadata().map_or(0, |m| m.len()),
            _ => 0, // symlinks are not followed
        })
        .sum()
}

/// Deletes `dir` with everything in it; a missing folder is fine.
pub fn clear(dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Deletes the files in `dir` whose names do not start with one of `keep`
/// (so a kept file's `.part` stays too). Returns how many were deleted.
pub fn remove_unused(dir: &Path, keep: &[String]) -> io::Result<usize> {
    let mut removed = 0;
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        other => other?,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_file = entry.file_type().is_ok_and(|t| t.is_file());
        if is_file && !keep.iter().any(|k| name.starts_with(k.as_str())) {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// "0 B", "820 KB", "46.2 MB", "1.3 GB" (units of 1024).
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        1 => format!("{value:.0} KB"),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
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

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openzap-cache-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn size_and_clear() {
        let dir = temp_dir("size");
        std::fs::create_dir_all(dir.join("logos")).unwrap();
        std::fs::write(dir.join("logos/a.img"), [0u8; 100]).unwrap();
        std::fs::write(dir.join("b.epg"), [0u8; 50]).unwrap();
        assert_eq!(size(&dir), 150);
        clear(&dir).unwrap();
        assert_eq!(size(&dir), 0);
        clear(&dir).unwrap(); // already gone: fine
    }

    #[test]
    fn unused_files_are_removed() {
        let dir = temp_dir("unused");
        for name in ["aaa.epg", "aaa.epg.part", "bbb.playlist", "old.epg"] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        let keep = ["aaa.epg".to_string(), "bbb.playlist".to_string()];
        assert_eq!(remove_unused(&dir, &keep).unwrap(), 1);
        assert!(!dir.join("old.epg").exists());
        assert!(
            dir.join("aaa.epg.part").exists(),
            "a download in progress stays"
        );
        assert_eq!(remove_unused(&dir.join("missing"), &keep).unwrap(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(820 * 1024), "820 KB");
        assert_eq!(format_size(45_397_688), "43.3 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
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
