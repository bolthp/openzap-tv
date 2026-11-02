//! Persistent user settings in a tiny `key=value` text file.
//!
//! The file may contain URLs with access keys, so it is created readable
//! by the owner only (on Unix) and its content is never logged.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::player::{HardwareDecoding, PlaybackSettings};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Playlist path or URL used last time.
    pub playlist: Option<String>,
    /// EPG path or URL used last time.
    pub epg: Option<String>,
    /// Volume in percent, 0–100.
    pub volume: f64,
    /// Keys (`Channel::key`) of favorite channels. A `BTreeSet` keeps the
    /// file sorted and free of duplicates.
    pub favorites: BTreeSet<String>,
    /// Key of the channel played last.
    pub last_channel: Option<String>,
    /// Start playing `last_channel` when the app starts.
    pub resume_last: bool,
    pub playback: PlaybackSettings,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            playlist: None,
            epg: None,
            volume: 80.0,
            favorites: BTreeSet::new(),
            last_channel: None,
            resume_last: false,
            playback: PlaybackSettings::default(),
        }
    }
}

impl Config {
    /// Lenient: unknown keys, comments and malformed lines are ignored.
    pub fn parse(text: &str) -> Self {
        let mut cfg = Config::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "playlist" if !value.is_empty() => cfg.playlist = Some(value.to_string()),
                "epg" if !value.is_empty() => cfg.epg = Some(value.to_string()),
                "favorite" if !value.is_empty() => {
                    cfg.favorites.insert(value.to_string());
                }
                "last_channel" if !value.is_empty() => cfg.last_channel = Some(value.to_string()),
                "resume_last" => cfg.resume_last = matches!(value, "yes" | "true" | "1"),
                "hwdec" => {
                    // Unknown values keep the default.
                    match value {
                        "auto" => cfg.playback.hwdec = HardwareDecoding::Auto,
                        "off" => cfg.playback.hwdec = HardwareDecoding::Off,
                        _ => {}
                    }
                }
                "buffer_secs" => {
                    if let Ok(secs) = value.parse::<u32>() {
                        cfg.playback.buffer_secs = secs.clamp(
                            PlaybackSettings::MIN_BUFFER_SECS,
                            PlaybackSettings::MAX_BUFFER_SECS,
                        );
                    }
                }
                "volume" => {
                    if let Ok(v) = value.parse::<f64>()
                        && v.is_finite()
                    {
                        cfg.volume = v.clamp(0.0, 100.0);
                    }
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn serialize(&self) -> String {
        let mut out = String::from("# OpenZap settings\n");
        // Values are single-line by construction; strip newlines defensively.
        let one_line = |s: &str| s.replace(['\r', '\n'], "");
        if let Some(p) = &self.playlist {
            out.push_str(&format!("playlist={}\n", one_line(p)));
        }
        if let Some(e) = &self.epg {
            out.push_str(&format!("epg={}\n", one_line(e)));
        }
        out.push_str(&format!("volume={}\n", self.volume));
        out.push_str(&format!(
            "resume_last={}\n",
            if self.resume_last { "yes" } else { "no" }
        ));
        let hwdec = match self.playback.hwdec {
            HardwareDecoding::Off => "off",
            HardwareDecoding::Auto => "auto",
        };
        out.push_str(&format!("hwdec={hwdec}\n"));
        out.push_str(&format!("buffer_secs={}\n", self.playback.buffer_secs));
        if let Some(c) = &self.last_channel {
            out.push_str(&format!("last_channel={}\n", one_line(c)));
        }
        for key in &self.favorites {
            out.push_str(&format!("favorite={}\n", one_line(key)));
        }
        out
    }

    /// Reads settings; a missing or unreadable file gives the defaults.
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|t| Config::parse(&t))
            .unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)?.write_all(self.serialize().as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Unix,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Unix
        }
    }
}

/// Location of the settings file, given an environment lookup function.
///
/// Taking the lookup as a parameter keeps this testable without touching
/// the real environment.
pub fn config_path(platform: Platform, env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let non_empty = |name: &str| env(name).filter(|v| !v.is_empty());
    let base = match platform {
        Platform::Windows => PathBuf::from(non_empty("APPDATA")?).join("OpenZap"),
        Platform::MacOs => PathBuf::from(non_empty("HOME")?)
            .join("Library")
            .join("Application Support")
            .join("OpenZap"),
        Platform::Unix => match non_empty("XDG_CONFIG_HOME") {
            Some(dir) => PathBuf::from(dir).join("openzap"),
            None => PathBuf::from(non_empty("HOME")?)
                .join(".config")
                .join("openzap"),
        },
    };
    Some(base.join("config.txt"))
}

/// Settings file location for the current platform and environment.
pub fn default_path() -> Option<PathBuf> {
    config_path(Platform::current(), |name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn roundtrip() {
        let cfg = Config {
            playlist: Some("https://example.com/list.m3u?token=fakekey".into()),
            epg: Some("/home/me/epg.xml.gz".into()),
            volume: 42.5,
            favorites: ["news.example".to_string(), "Sport = One".to_string()].into(),
            last_channel: Some("news.example".into()),
            resume_last: true,
            playback: PlaybackSettings {
                hwdec: HardwareDecoding::Auto,
                buffer_secs: 25,
            },
        };
        assert_eq!(Config::parse(&cfg.serialize()), cfg);
    }

    #[test]
    fn resume_last_accepts_common_spellings() {
        for yes in ["yes", "true", "1"] {
            assert!(Config::parse(&format!("resume_last={yes}")).resume_last);
        }
        for no in ["no", "false", "0", "maybe"] {
            assert!(!Config::parse(&format!("resume_last={no}")).resume_last);
        }
    }

    #[test]
    fn playback_settings_parse_leniently() {
        let cfg = Config::parse("hwdec=off\nbuffer_secs=0\n");
        assert_eq!(cfg.playback.hwdec, HardwareDecoding::Off);
        assert_eq!(cfg.playback.buffer_secs, PlaybackSettings::MIN_BUFFER_SECS);
        let cfg = Config::parse("hwdec=bogus\nbuffer_secs=lots\n");
        assert_eq!(cfg.playback, PlaybackSettings::default());
        assert_eq!(PlaybackSettings::default().hwdec, HardwareDecoding::Auto);
    }

    #[test]
    fn duplicate_favorites_collapse() {
        let cfg = Config::parse("favorite=a\nfavorite=b\nfavorite=a\nfavorite=\n");
        assert_eq!(cfg.favorites.len(), 2);
    }

    #[test]
    fn defaults_when_empty() {
        assert_eq!(Config::parse(""), Config::default());
    }

    #[test]
    fn value_may_contain_equals_signs() {
        let cfg = Config::parse("playlist=http://example.com/a?x=1&y=2\n");
        assert_eq!(
            cfg.playlist.as_deref(),
            Some("http://example.com/a?x=1&y=2")
        );
    }

    #[test]
    fn ignores_junk_and_clamps_volume() {
        let cfg = Config::parse("# c\nnonsense\nunknown=1\nvolume=250\nplaylist=\n");
        assert_eq!(cfg.volume, 100.0);
        assert_eq!(cfg.playlist, None);
        assert_eq!(Config::parse("volume=NaN").volume, 80.0);
        assert_eq!(Config::parse("volume=abc").volume, 80.0);
    }

    #[test]
    fn newlines_cannot_inject_keys() {
        let cfg = Config {
            playlist: Some("a\nepg=evil".into()),
            ..Config::default()
        };
        assert_eq!(Config::parse(&cfg.serialize()).epg, None);
    }

    #[test]
    fn paths_per_platform() {
        let unix = env_of(&[("HOME", "/home/me")]);
        assert_eq!(
            config_path(Platform::Unix, unix),
            Some(PathBuf::from("/home/me/.config/openzap/config.txt"))
        );
        let xdg = env_of(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "/cfg")]);
        assert_eq!(
            config_path(Platform::Unix, xdg),
            Some(PathBuf::from("/cfg/openzap/config.txt"))
        );
        let mac = env_of(&[("HOME", "/Users/me")]);
        assert_eq!(
            config_path(Platform::MacOs, mac),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/OpenZap/config.txt"
            ))
        );
        let win = env_of(&[("APPDATA", "C:\\Users\\me\\AppData\\Roaming")]);
        assert_eq!(
            config_path(Platform::Windows, win),
            Some(PathBuf::from("C:\\Users\\me\\AppData\\Roaming").join("OpenZap/config.txt"))
        );
    }

    #[test]
    fn no_path_without_home() {
        assert_eq!(config_path(Platform::Unix, env_of(&[])), None);
        assert_eq!(
            config_path(Platform::Windows, env_of(&[("APPDATA", "")])),
            None
        );
    }

    #[test]
    fn save_and_load_file() {
        let dir = std::env::temp_dir().join(format!("openzap-cfg-{}", std::process::id()));
        let path = dir.join("nested").join("config.txt");
        let cfg = Config {
            playlist: Some("http://example.com/x".into()),
            epg: None,
            volume: 10.0,
            ..Config::default()
        };
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), cfg);
        assert_eq!(
            Config::load_from(&dir.join("missing.txt")),
            Config::default()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "config must not be group/world readable");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
