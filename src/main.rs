// No console window for release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod video;

use std::path::PathBuf;
use std::sync::Arc;

use openzap::debuglog::DebugLog;
use openzap::platform;

const DEBUG_FLAG: &str = "--debug";
const DEBUG_LOG_NAME: &str = "openzap-debug.log";

/// Removes `--debug` from the arguments; returns whether it was present.
fn take_debug_flag(args: &mut Vec<String>) -> bool {
    let before = args.len();
    args.retain(|a| a != DEBUG_FLAG);
    args.len() != before
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

    // Usage: openzap [--debug] [PLAYLIST.m3u [EPG.xml]]
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let debug_log = take_debug_flag(&mut args).then(open_debug_log).flatten();
    if let Some(log) = &debug_log {
        log.line(&format!(
            "OpenZap {} on {}/{}, args: {:?}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            args
        ));
        let panic_log = Arc::clone(log);
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            panic_log.line(&format!("PANIC: {info}"));
            default_hook(info);
        }));
    }

    let mut app = app::App::from_args(&args);
    if let Some(log) = &debug_log {
        app.set_debug_log(Arc::clone(log));
    }
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("OpenZap")
        .with_inner_size([1280.0, 760.0])
        .with_min_inner_size([720.0, 480.0]);
    // The PNG is compiled in, so this only fails if the file in the repo is broken.
    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        Ok(icon) => viewport = viewport.with_icon(std::sync::Arc::new(icon)),
        Err(e) => eprintln!("window icon unavailable: {e}"),
    }
    let options = eframe::NativeOptions {
        // mpv renders video through OpenGL, so egui must use it too.
        renderer: eframe::Renderer::Glow,
        viewport,
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

#[cfg(test)]
mod tests {
    use super::*;

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
