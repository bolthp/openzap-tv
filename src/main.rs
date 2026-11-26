// No console window for release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod filepick;
mod gamepad;
mod settings;
mod video;

use std::path::PathBuf;
use std::sync::Arc;

use openzap::debuglog::DebugLog;
use openzap::mask::mask_urls_in_text;
use openzap::platform;

const DEBUG_FLAG: &str = "--debug";
/// Where to put the window on start and how big, in screen pixels (e.g. on
/// another monitor); they override the geometry saved at the last exit.
const POSITION_FLAG: &str = "--position";
const SIZE_FLAG: &str = "--size";
const DEBUG_LOG_NAME: &str = "openzap-debug.log";

/// Removes `--debug` from the arguments; returns whether it was present.
fn take_debug_flag(args: &mut Vec<String>) -> bool {
    let before = args.len();
    args.retain(|a| a != DEBUG_FLAG);
    args.len() != before
}

/// Two numbers from a flag (`None`: not given), or why they are bad.
type FlagPair = Result<Option<(i32, i32)>, String>;

/// Removes `FLAG A<sep>B` (or `FLAG=A<sep>B`) from the arguments and
/// returns the two numbers; `Err` names the expected `form` if the value
/// is bad.
fn take_pair(args: &mut Vec<String>, flag: &str, sep: char, form: &str) -> FlagPair {
    let Some(i) = args
        .iter()
        .position(|a| a == flag || a.starts_with(&format!("{flag}=")))
    else {
        return Ok(None);
    };
    let arg = args.remove(i);
    let value = match arg.split_once('=') {
        Some((_, v)) => v.to_string(),
        None if i < args.len() => args.remove(i),
        None => return Err(format!("{flag} needs {form}")),
    };
    let parsed = value
        .split_once(sep)
        .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)));
    parsed
        .map(Some)
        .ok_or_else(|| format!("{flag}: \"{value}\" is not {form} in pixels"))
}

/// `--position X,Y`: the window's top-left corner.
fn take_position(args: &mut Vec<String>) -> FlagPair {
    take_pair(args, POSITION_FLAG, ',', "X,Y")
}

/// `--size WxH`: the window's inner size.
fn take_size(args: &mut Vec<String>) -> FlagPair {
    match take_pair(args, SIZE_FLAG, 'x', "WxH")? {
        Some((w, h)) if w <= 0 || h <= 0 => Err(format!("{SIZE_FLAG}: {w}x{h} is not a size")),
        size => Ok(size),
    }
}

/// The command line as written to the debug log: all arguments, `--debug`
/// included, with access keys in URLs masked.
fn logged_args(args: &[String]) -> Vec<String> {
    args.iter().map(|a| mask_urls_in_text(a)).collect()
}

/// The session's kind for the debug log: the variables that tell a
/// desktop from Steam Deck's Game Mode, and which file dialog is used.
fn session_line(env: impl Fn(&str) -> Option<String>) -> String {
    let vars: Vec<String> = openzap::desktop::SESSION_VARS
        .iter()
        .filter_map(|name| env(name).map(|v| format!("{name}={v}")))
        .collect();
    let dialog = if openzap::desktop::builtin_file_browser(&env) {
        "built-in"
    } else {
        "system"
    };
    format!("session: {} -> file dialog: {dialog}", vars.join(" "))
}

/// Opens the debug log next to the executable (the portable folder), or
/// in the settings directory, or in the temp directory, whichever works.
fn open_debug_log() -> Option<Arc<DebugLog>> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(PathBuf::from));
    let config_dir = openzap::config::default_path().and_then(|p| p.parent().map(PathBuf::from));
    [exe_dir, config_dir, Some(std::env::temp_dir())]
        .into_iter()
        .flatten()
        .find_map(|dir| {
            let _ = std::fs::create_dir_all(&dir);
            DebugLog::create(&dir.join(DEBUG_LOG_NAME)).ok()
        })
        .map(Arc::new)
}

fn main() -> eframe::Result {
    if platform::should_prefer_x11(platform::is_wsl(), std::env::var_os("DISPLAY").is_some()) {
        // SAFETY: no other threads exist yet, so changing the environment
        // cannot race with readers.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }

    // Usage: openzap [--debug] [--position X,Y] [PLAYLIST.m3u [EPG.xml]]
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let given = logged_args(&args);
    let debug_log = take_debug_flag(&mut args).then(open_debug_log).flatten();
    if let Some(log) = &debug_log {
        log.line(&format!(
            "OpenZap {} on {}/{}, args: {:?}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            given
        ));
        log.line(&session_line(|name| std::env::var(name).ok()));
        let panic_log = Arc::clone(log);
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            panic_log.line(&format!("PANIC: {info}"));
            default_hook(info);
        }));
    }

    let mut flag_value = |take: fn(&mut Vec<String>) -> FlagPair| {
        take(&mut args).unwrap_or_else(|e| {
            eprintln!("{e}");
            if let Some(log) = &debug_log {
                log.line(&e);
            }
            None
        })
    };
    let start_position = flag_value(take_position);
    let start_size = flag_value(take_size);
    let mut app = app::App::from_args(&args);
    app.set_start_geometry(start_position, start_size);
    if let Some(log) = &debug_log {
        app.set_debug_log(Arc::clone(log));
    }
    let env = |name: &str| std::env::var(name).ok();
    // A Flatpak exports its own desktop entry.
    #[cfg(all(unix, not(target_os = "macos")))]
    if openzap::desktop::flatpak_id(env).is_none() {
        install_desktop_entry(debug_log.as_deref());
    }

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("OpenZap")
        // Lets Wayland compositors find the `.desktop` file for the icon.
        .with_app_id(openzap::desktop::app_id(env))
        .with_inner_size([1280.0, 760.0])
        .with_min_inner_size(app::MIN_WINDOW_SIZE);
    // The PNG is compiled in, so this only fails if the file in the repo is broken.
    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        Ok(icon) => viewport = viewport.with_icon(std::sync::Arc::new(icon)),
        Err(e) => eprintln!("window icon unavailable: {e}"),
    }
    if let Some((x, y)) = start_position {
        // Created right there, the window does not hop between monitors
        // (and scales) after start. Pixels are taken as points here; the
        // app corrects the position once the scale is known.
        viewport = viewport.with_position([x as f32, y as f32]);
    }
    if let Some((w, h)) = start_size {
        // Corrected for the scale once the window is up, like the position.
        viewport = viewport
            .with_inner_size([w as f32, h as f32])
            .with_min_inner_size([0.0, 0.0]);
    }
    let options = eframe::NativeOptions {
        // mpv renders video through OpenGL, so egui must use it too.
        renderer: eframe::Renderer::Glow,
        viewport,
        // Window size and position (eframe's `persistence` feature).
        persistence_path: openzap::config::default_path()
            .map(|p| openzap::config::window_state_path(&p)),
        // The saved geometry would win over `--position` / `--size`.
        persist_window: start_position.is_none() && start_size.is_none(),
        ..Default::default()
    };
    let result = eframe::run_native(
        "OpenZap",
        options,
        Box::new(move |cc| {
            app.set_context(cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    );
    if let Some(log) = &debug_log {
        log.line(&format!("exit: {result:?}"));
    }
    result
}

/// Writes `~/.local/share/applications/openzap.desktop` (and the icon) so
/// the taskbar shows OpenZap's icon; on Wayland the window cannot set it.
/// Failures only cost the icon, so they are just logged.
#[cfg(all(unix, not(target_os = "macos")))]
fn install_desktop_entry(log: Option<&DebugLog>) {
    let Some(data_home) = openzap::desktop::data_home(|n| std::env::var(n).ok()) else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let result = openzap::desktop::install(&data_home, &exe, include_bytes!("../assets/icon.png"));
    if let Some(log) = log {
        log.line(&format!("desktop entry: {result:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_flag_is_taken_with_its_value() {
        let strings = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut args = strings(&["a.m3u", "--position", "2560,16", "e.xml"]);
        assert_eq!(take_position(&mut args), Ok(Some((2560, 16))));
        assert_eq!(args, ["a.m3u", "e.xml"]);
        let mut args = strings(&["--position=-1920, 0"]);
        assert_eq!(take_position(&mut args), Ok(Some((-1920, 0))));
        assert!(args.is_empty());
        let mut args = strings(&["a.m3u"]);
        assert_eq!(take_position(&mut args), Ok(None));
        assert_eq!(args, ["a.m3u"]);
        for bad in [
            &["--position"][..],
            &["--position", "left"],
            &["--position=1"],
        ] {
            let mut args = strings(bad);
            assert!(take_position(&mut args).is_err(), "{bad:?}");
            assert!(args.is_empty(), "the flag is removed: {bad:?}");
        }
    }

    #[test]
    fn size_flag_takes_width_x_height() {
        let strings = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut args = strings(&["--size", "1600x900", "a.m3u"]);
        assert_eq!(take_size(&mut args), Ok(Some((1600, 900))));
        assert_eq!(args, ["a.m3u"]);
        assert_eq!(
            take_size(&mut strings(&["--size=800x600"])),
            Ok(Some((800, 600)))
        );
        assert_eq!(take_size(&mut strings(&[])), Ok(None));
        for bad in ["1600,900", "0x600", "-5x10", "big"] {
            assert!(take_size(&mut strings(&["--size", bad])).is_err(), "{bad}");
        }
    }

    #[test]
    fn session_line_names_the_variables_and_the_dialog() {
        let env = |name: &str| (name == "XDG_CURRENT_DESKTOP").then(|| "gamescope".to_string());
        assert_eq!(
            session_line(env),
            "session: XDG_CURRENT_DESKTOP=gamescope -> file dialog: built-in"
        );
        assert_eq!(session_line(|_| None), "session:  -> file dialog: system");
    }

    #[test]
    fn logged_args_keep_the_flag_and_hide_keys() {
        let args = [
            "--debug".to_string(),
            "http://example.com/get.php?username=user&password=secret123".to_string(),
        ];
        let logged = logged_args(&args);
        assert_eq!(logged[0], "--debug");
        assert!(!logged[1].contains("secret123"), "{}", logged[1]);
        assert!(logged[1].starts_with("http://example.com/"));
    }

    #[test]
    fn debug_flag_is_removed_anywhere() {
        let mut args = vec![
            "a.m3u".to_string(),
            "--debug".to_string(),
            "e.xml".to_string(),
        ];
        assert!(take_debug_flag(&mut args));
        assert_eq!(args, ["a.m3u", "e.xml"]);

        let mut args = vec!["a.m3u".to_string()];
        assert!(!take_debug_flag(&mut args));
        assert_eq!(args, ["a.m3u"]);
    }
}
