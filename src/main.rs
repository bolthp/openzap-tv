mod app;

fn main() -> eframe::Result {
    // Usage: openzap [PLAYLIST.m3u [EPG.xml]]
    let args: Vec<String> = std::env::args().skip(1).collect();
    let app = app::App::from_args(&args);
    eframe::run_native(
        "OpenZap",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(app))),
    )
}
