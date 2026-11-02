// No console window for release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;

use openzap::platform;

fn main() -> eframe::Result {
    if platform::should_prefer_x11(platform::is_wsl(), std::env::var_os("DISPLAY").is_some()) {
        // SAFETY: no other threads exist yet, so changing the environment
        // cannot race with readers.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }

    // Usage: openzap [PLAYLIST.m3u [EPG.xml]]
    let args: Vec<String> = std::env::args().skip(1).collect();
    let app = app::App::from_args(&args);
    eframe::run_native(
        "OpenZap",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(app))),
    )
}
