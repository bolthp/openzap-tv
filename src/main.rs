// No console window for release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod video;

use openzap::platform;

fn main() -> eframe::Result {
    if platform::should_prefer_x11(platform::is_wsl(), std::env::var_os("DISPLAY").is_some()) {
        // SAFETY: no other threads exist yet, so changing the environment
        // cannot race with readers.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }

    // Usage: openzap [PLAYLIST.m3u [EPG.xml]]
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut app = app::App::from_args(&args);
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
    eframe::run_native(
        "OpenZap",
        options,
        Box::new(move |cc| {
            app.set_context(cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    )
}
