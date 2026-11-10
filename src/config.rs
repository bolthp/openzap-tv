//! Persistent user settings in a tiny `key=value` text file.
//!
//! The file may contain URLs with access keys, so it is created readable
//! by the owner only (on Unix) and its content is never logged.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::player::{AspectMode, HardwareDecoding, PlaybackSettings};

/// Which side of the window the channel list is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    #[default]
    Left,
    Right,
}

/// Colour theme of the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Follow the operating system.
    #[default]
    System,
    Dark,
    Light,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Dark, Theme::Light];

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Dark => "Dark",
            Theme::Light => "Light",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }
}

/// When a playlist or guide from the internet is downloaded again; until
/// then the copy saved last time is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePolicy {
    OnStart,
    Daily,
    /// Only with the reload button (or when there is no saved copy).
    Never,
}

impl UpdatePolicy {
    pub const ALL: [UpdatePolicy; 3] = [
        UpdatePolicy::OnStart,
        UpdatePolicy::Daily,
        UpdatePolicy::Never,
    ];

    pub fn label(self) -> &'static str {
        match self {
            UpdatePolicy::OnStart => "On every start",
            UpdatePolicy::Daily => "Once a day",
            UpdatePolicy::Never => "Never (reload button only)",
        }
    }

    fn key(self) -> &'static str {
        match self {
            UpdatePolicy::OnStart => "start",
            UpdatePolicy::Daily => "daily",
            UpdatePolicy::Never => "never",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == value)
    }
}

/// Window layout preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewSettings {
    pub list_side: Side,
    pub list_visible: bool,
    pub always_on_top: bool,
    pub theme: Theme,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            list_side: Side::Left,
            list_visible: true,
            always_on_top: false,
            theme: Theme::System,
        }
    }
}

fn aspect_key(mode: AspectMode) -> &'static str {
    match mode {
        AspectMode::Auto => "auto",
        AspectMode::Ratio16x9 => "16:9",
        AspectMode::Ratio4x3 => "4:3",
        AspectMode::Fill => "fill",
        AspectMode::Stretch => "stretch",
    }
}

fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "yes" | "true" | "1" => Some(true),
        "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

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
    pub view: ViewSettings,
    pub playlist_update: UpdatePolicy,
    pub epg_update: UpdatePolicy,
    /// Picture size chosen for a channel (by `Channel::key`); channels not
    /// listed use `AspectMode::Auto`.
    pub channel_aspect: BTreeMap<String, AspectMode>,
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
            view: ViewSettings::default(),
            // Playlists are small and their links may change; guides are
            // big and published once a day.
            playlist_update: UpdatePolicy::OnStart,
            epg_update: UpdatePolicy::Daily,
            channel_aspect: BTreeMap::new(),
        }
    }
}

impl Config {
    /// The picture size for the channel with this key.
    pub fn aspect_for(&self, channel_key: &str) -> AspectMode {
        self.channel_aspect
            .get(channel_key)
            .copied()
            .unwrap_or(AspectMode::Auto)
    }

    /// Remembers the picture size for a channel (`Auto` forgets it).
    pub fn set_aspect_for(&mut self, channel_key: &str, mode: AspectMode) {
        if mode == AspectMode::Auto {
            self.channel_aspect.remove(channel_key);
        } else {
            self.channel_aspect.insert(channel_key.to_string(), mode);
        }
    }

    /// Lenient: unknown keys, comments and malformed lines are ignored.
    pub fn parse(text: &str) -> Self {
        let mut cfg = Config::default();
        // Notepad and PowerShell may save UTF-8 with a byte-order mark.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
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
                "list_side" => match value {
                    "left" => cfg.view.list_side = Side::Left,
                    "right" => cfg.view.list_side = Side::Right,
                    _ => {}
                },
                "list_visible" => {
                    if let Some(v) = parse_bool(value) {
                        cfg.view.list_visible = v;
                    }
                }
                "always_on_top" => {
                    if let Some(v) = parse_bool(value) {
                        cfg.view.always_on_top = v;
                    }
                }
                // "<mode> <channel key>": the key may contain anything.
                "channel_aspect" => {
                    if let Some((mode, key)) = value.split_once(' ')
                        && let Some(&mode) =
                            AspectMode::ALL.iter().find(|&&m| aspect_key(m) == mode)
                        && !key.trim().is_empty()
                    {
                        cfg.set_aspect_for(key.trim(), mode);
                    }
                }
                "theme" => {
                    if let Some(&t) = Theme::ALL.iter().find(|t| t.key() == value) {
                        cfg.view.theme = t;
                    }
                }
                "playlist_update" => {
                    if let Some(p) = UpdatePolicy::parse(value) {
                        cfg.playlist_update = p;
                    }
                }
                "epg_update" => {
                    if let Some(p) = UpdatePolicy::parse(value) {
                        cfg.epg_update = p;
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
        out.push_str(&format!("resume_last={}\n", yes_no(self.resume_last)));
        out.push_str(&format!("playlist_update={}\n", self.playlist_update.key()));
        out.push_str(&format!("epg_update={}\n", self.epg_update.key()));
        let hwdec = match self.playback.hwdec {
            HardwareDecoding::Off => "off",
            HardwareDecoding::Auto => "auto",
        };
        out.push_str(&format!("hwdec={hwdec}\n"));
        out.push_str(&format!("buffer_secs={}\n", self.playback.buffer_secs));
        let side = match self.view.list_side {
            Side::Left => "left",
            Side::Right => "right",
        };
        out.push_str(&format!("list_side={side}\n"));
        out.push_str(&format!(
            "list_visible={}\n",
            yes_no(self.view.list_visible)
        ));
        out.push_str(&format!(
            "always_on_top={}\n",
            yes_no(self.view.always_on_top)
        ));
        out.push_str(&format!("theme={}\n", self.view.theme.key()));
        if let Some(c) = &self.last_channel {
            out.push_str(&format!("last_channel={}\n", one_line(c)));
        }
        for key in &self.favorites {
            out.push_str(&format!("favorite={}\n", one_line(key)));
        }
        for (key, mode) in &self.channel_aspect {
            out.push_str(&format!(
                "channel_aspect={} {}\n",
                aspect_key(*mode),
                one_line(key)
            ));
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
        crate::cache::create_private(path)?.write_all(self.serialize().as_bytes())
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

/// Where the window size and position are kept: `window.ron` next to the
/// settings file. eframe treats this as a file path and creates its folder.
pub fn window_state_path(config: &Path) -> PathBuf {
    config.with_file_name("window.ron")
}

/// Settings file location for the current platform and environment.
pub fn default_path() -> Option<PathBuf> {
    config_path(Platform::current(), |name| std::env::var(name).ok())
}

/// Inside a Flatpak, settings live in the app's own folder; this is where
/// the plain (tar.gz) build keeps them, to start from on the first run.
/// `None` outside a Flatpak.
pub fn host_config_path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let non_empty = |name: &str| env(name).filter(|v| !v.trim().is_empty());
    non_empty("FLATPAK_ID")?;
    Some(
        PathBuf::from(non_empty("HOME")?)
            .join(".config")
            .join("openzap")
            .join("config.txt"),
    )
}

/// Reads the settings from `path`; if there are none yet, from `fallback`
/// (see [`host_config_path`]). Saving always goes to `path`.
pub fn load_or_import(path: &Path, fallback: Option<&Path>) -> Config {
    match fallback {
        Some(old) if !path.exists() && old.exists() => Config::load_from(old),
        _ => Config::load_from(path),
    }
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
            view: ViewSettings {
                list_side: Side::Right,
                list_visible: false,
                always_on_top: true,
                theme: Theme::Dark,
            },
            playlist_update: UpdatePolicy::Never,
            epg_update: UpdatePolicy::OnStart,
            channel_aspect: [
                ("news.example".to_string(), AspectMode::Fill),
                ("Sport = One HD".to_string(), AspectMode::Ratio4x3),
            ]
            .into(),
        };
        assert_eq!(Config::parse(&cfg.serialize()), cfg);
    }

    #[test]
    fn update_policies_parse_leniently() {
        for p in UpdatePolicy::ALL {
            let cfg = Config::parse(&format!("playlist_update={0}\nepg_update={0}\n", p.key()));
            assert_eq!((cfg.playlist_update, cfg.epg_update), (p, p));
        }
        let cfg = Config::parse("playlist_update=hourly\nepg_update=\n");
        assert_eq!(cfg.playlist_update, UpdatePolicy::OnStart);
        assert_eq!(cfg.epg_update, UpdatePolicy::Daily);
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
    fn view_settings_parse_leniently() {
        assert_eq!(Config::parse("").view, ViewSettings::default());
        for theme in Theme::ALL {
            let text = format!("theme={}\n", theme.key());
            assert_eq!(Config::parse(&text).view.theme, theme);
        }
        let cfg = Config::parse("list_side=top\nlist_visible=maybe\naspect=21:9\ntheme=pink\n");
        assert_eq!(cfg.view, ViewSettings::default());
    }

    #[test]
    fn picture_size_is_kept_per_channel() {
        let mut cfg = Config::default();
        for mode in AspectMode::ALL {
            cfg.set_aspect_for("a", mode);
            assert_eq!(Config::parse(&cfg.serialize()).aspect_for("a"), mode);
        }
        cfg.set_aspect_for("a", AspectMode::Auto);
        assert!(cfg.channel_aspect.is_empty(), "Auto is not stored");
        assert_eq!(cfg.aspect_for("unknown"), AspectMode::Auto);
        // Broken lines and the old global setting are ignored.
        let cfg = Config::parse("channel_aspect=fill\nchannel_aspect=huge x\naspect=fill\n");
        assert!(cfg.channel_aspect.is_empty());
    }

    #[test]
    fn duplicate_favorites_collapse() {
        let cfg = Config::parse("favorite=a\nfavorite=b\nfavorite=a\nfavorite=\n");
        assert_eq!(cfg.favorites.len(), 2);
    }

    #[test]
    fn byte_order_mark_is_ignored() {
        let cfg = Config::parse("\u{feff}playlist=/data/list.m3u\r\nvolume=10\r\n");
        assert_eq!(cfg.playlist.as_deref(), Some("/data/list.m3u"));
        assert_eq!(cfg.volume, 10.0);
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
    fn window_state_lives_next_to_the_settings() {
        assert_eq!(
            window_state_path(Path::new("/home/me/.config/openzap/config.txt")),
            PathBuf::from("/home/me/.config/openzap/window.ron")
        );
    }

    #[test]
    fn flatpak_starts_from_the_plain_builds_settings() {
        let env = env_of(&[("HOME", "/home/me"), ("FLATPAK_ID", "io.example.App")]);
        assert_eq!(
            host_config_path(env),
            Some(PathBuf::from("/home/me/.config/openzap/config.txt"))
        );
        assert_eq!(host_config_path(env_of(&[("HOME", "/home/me")])), None);

        let dir = std::env::temp_dir().join(format!("openzap-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (own, old) = (dir.join("own.txt"), dir.join("old.txt"));
        std::fs::write(&old, "volume=11\n").unwrap();
        assert_eq!(load_or_import(&own, Some(&old)).volume, 11.0, "first run");
        std::fs::write(&own, "volume=22\n").unwrap();
        assert_eq!(
            load_or_import(&own, Some(&old)).volume,
            22.0,
            "own settings win"
        );
        assert_eq!(load_or_import(&own, None).volume, 22.0);
        std::fs::remove_dir_all(&dir).unwrap();
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
