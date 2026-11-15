//! Linux desktop integration: a `.desktop` entry and icon in the user's
//! data directory, so launchers and the taskbar show OpenZap's icon.
//!
//! On Wayland a window cannot set its own icon; the compositor looks up
//! the `.desktop` file whose name matches the window's app id. Without
//! one, the taskbar shows a generic icon. Packages install this file
//! system-wide; for the plain tar.gz the app writes it on start-up (as
//! e.g. Telegram's tarball does). A Flatpak exports its own entry, named
//! after the Flatpak id.

use std::io;
use std::path::{Path, PathBuf};

/// Window app id (Wayland) / WM_CLASS (X11); also the `.desktop` file name.
pub const APP_ID: &str = "openzap";

/// The Flatpak id when running inside a Flatpak (`FLATPAK_ID` is set by it).
pub fn flatpak_id(env: impl Fn(&str) -> Option<String>) -> Option<String> {
    env("FLATPAK_ID").filter(|id| !id.trim().is_empty())
}

/// Environment variables that tell what kind of session this is, for
/// the debug log.
pub const SESSION_VARS: [&str; 6] = [
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "GAMESCOPE_WAYLAND_DISPLAY",
    "SteamDeck",
    "SteamGamepadUI",
    "OPENZAP_FILE_BROWSER",
];

/// Whether the system's "Open file" dialog is unusable and the built-in
/// file browser should be used instead: in Steam Deck's Game Mode
/// (gamescope session) no portal backend shows one. `OPENZAP_FILE_BROWSER`
/// set to `builtin` or `system` overrides the guess.
pub fn builtin_file_browser(env: impl Fn(&str) -> Option<String>) -> bool {
    match env("OPENZAP_FILE_BROWSER").as_deref() {
        Some("builtin") => return true,
        Some("system") => return false,
        _ => {}
    }
    let gamescope_desktop = ["XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP"]
        .iter()
        .filter_map(|name| env(name))
        .any(|v| v.to_ascii_lowercase().contains("gamescope"));
    gamescope_desktop || env("SteamGamepadUI").as_deref() == Some("1")
}

/// The window's app id: the Flatpak id inside a Flatpak (its exported
/// `.desktop` file has that name), else [`APP_ID`].
pub fn app_id(env: impl Fn(&str) -> Option<String>) -> String {
    flatpak_id(env).unwrap_or_else(|| APP_ID.to_string())
}

/// Where per-user `.desktop` files and icons live (XDG data home).
pub fn data_home(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let non_empty = |name: &str| env(name).filter(|v| !v.is_empty());
    match non_empty("XDG_DATA_HOME") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(
            PathBuf::from(non_empty("HOME")?)
                .join(".local")
                .join("share"),
        ),
    }
}

/// Paths of the entry and the icon under `data_home`.
pub fn paths(data_home: &Path) -> (PathBuf, PathBuf) {
    (
        data_home
            .join("applications")
            .join(format!("{APP_ID}.desktop")),
        data_home
            .join("icons")
            .join("hicolor")
            .join("256x256")
            .join("apps")
            .join(format!("{APP_ID}.png")),
    )
}

/// The `.desktop` entry starting `exe`, with the icon at `icon`.
pub fn entry(exe: &Path, icon: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=OpenZap\n\
         GenericName=IPTV Player\n\
         Comment=Watch IPTV playlists with a programme guide\n\
         Exec={}\n\
         Icon={}\n\
         Terminal=false\n\
         Categories=AudioVideo;Video;Player;TV;\n\
         StartupWMClass={APP_ID}\n",
        quote_exec(&exe.to_string_lossy()),
        icon.to_string_lossy().replace(['\n', '\r'], ""),
    )
}

/// Quotes a program path for `Exec=` (Desktop Entry Specification: inside
/// double quotes, `"`, `` ` ``, `$` and `\` are escaped with a backslash;
/// `\` itself is then escaped again as the value is a string).
fn quote_exec(path: &str) -> String {
    let mut out = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            '\n' | '\r' => {}
            '%' => out.push_str("%%"), // `%` starts field codes
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Writes the entry and icon under `data_home` unless they are already
/// up to date. Returns true if anything was written.
pub fn install(data_home: &Path, exe: &Path, icon_png: &[u8]) -> io::Result<bool> {
    let (entry_path, icon_path) = paths(data_home);
    let mut wrote = write_if_changed(&icon_path, icon_png)?;
    wrote |= write_if_changed(&entry_path, entry(exe, &icon_path).as_bytes())?;
    Ok(wrote)
}

fn write_if_changed(path: &Path, content: &[u8]) -> io::Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == content) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn builtin_file_browser_in_game_mode_only() {
        assert!(builtin_file_browser(vars(&[(
            "XDG_CURRENT_DESKTOP",
            "gamescope"
        )])));
        assert!(builtin_file_browser(vars(&[("SteamGamepadUI", "1")])));
        assert!(!builtin_file_browser(vars(&[(
            "XDG_CURRENT_DESKTOP",
            "KDE"
        )])));
        assert!(!builtin_file_browser(vars(&[("SteamDeck", "1")])));
        assert!(!builtin_file_browser(vars(&[])));
        // The override wins both ways.
        assert!(builtin_file_browser(vars(&[(
            "OPENZAP_FILE_BROWSER",
            "builtin"
        )])));
        assert!(!builtin_file_browser(vars(&[
            ("OPENZAP_FILE_BROWSER", "system"),
            ("XDG_CURRENT_DESKTOP", "gamescope"),
        ])));
    }
    use std::collections::HashMap;

    #[test]
    fn data_home_follows_xdg() {
        let env = |pairs: &'static [(&str, &str)]| {
            let map: HashMap<&str, &str> = pairs.iter().copied().collect();
            move |name: &str| map.get(name).map(|v| v.to_string())
        };
        assert_eq!(
            data_home(env(&[("HOME", "/home/me")])),
            Some(PathBuf::from("/home/me/.local/share"))
        );
        assert_eq!(
            data_home(env(&[("HOME", "/home/me"), ("XDG_DATA_HOME", "/data")])),
            Some(PathBuf::from("/data"))
        );
        assert_eq!(data_home(env(&[])), None);
    }

    #[test]
    fn app_id_is_the_flatpak_id_inside_a_flatpak() {
        let flatpak = |name: &str| (name == "FLATPAK_ID").then(|| "io.example.App".to_string());
        assert_eq!(app_id(flatpak), "io.example.App");
        assert_eq!(flatpak_id(flatpak).as_deref(), Some("io.example.App"));
        assert_eq!(app_id(|_| None), APP_ID);
        assert_eq!(flatpak_id(|_| Some(" ".to_string())), None);
    }

    #[test]
    fn entry_points_at_the_executable_and_icon() {
        let text = entry(
            Path::new("/opt/open zap/openzap"),
            Path::new("/home/me/.local/share/icons/hicolor/256x256/apps/openzap.png"),
        );
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(
            text.contains("\nExec=\"/opt/open zap/openzap\"\n"),
            "{text}"
        );
        assert!(
            text.contains("\nIcon=/home/me/.local/share/icons/hicolor/256x256/apps/openzap.png\n")
        );
        assert!(text.contains("\nStartupWMClass=openzap\n"));
    }

    #[test]
    fn exec_quoting_escapes_reserved_characters() {
        assert_eq!(quote_exec("/a/b"), "\"/a/b\"");
        assert_eq!(quote_exec("/a$b"), "\"/a\\\\$b\"");
        assert_eq!(quote_exec("/a\"b"), "\"/a\\\\\"b\"");
        assert_eq!(quote_exec("/100%"), "\"/100%%\"");
        assert_eq!(quote_exec("/a\nb"), "\"/ab\"");
    }

    #[test]
    fn install_writes_once_and_updates_when_moved() {
        let dir = std::env::temp_dir().join(format!("openzap-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let icon = b"png bytes";

        assert!(install(&dir, Path::new("/opt/a/openzap"), icon).unwrap());
        assert!(!install(&dir, Path::new("/opt/a/openzap"), icon).unwrap());
        // The app was moved: the entry follows it.
        assert!(install(&dir, Path::new("/opt/b/openzap"), icon).unwrap());

        let (entry_path, icon_path) = paths(&dir);
        assert!(
            std::fs::read_to_string(entry_path)
                .unwrap()
                .contains("/opt/b/openzap")
        );
        assert_eq!(std::fs::read(icon_path).unwrap(), icon);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
