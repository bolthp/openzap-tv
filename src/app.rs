//! egui application: channel browser with now/next details.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;
use openzap::browse::{Filter, filter_channels, format_duration, groups, progress};
use openzap::config::{self, Config};
use openzap::epg::{Epg, EpgChannel, Programme};
use openzap::guide::GuideIndex;
use openzap::loader::{self, Loaded};
use openzap::m3u::{Channel, Playlist};
use openzap::mask::mask_url;
use openzap::player::{Player, PlayerEvent};

/// A background load in progress, with the inputs it was started for.
struct PendingLoad {
    rx: Receiver<Loaded>,
    playlist_src: String,
    epg_src: String,
}

pub struct App {
    playlist: Playlist,
    epg: Epg,
    groups: Vec<String>,
    /// Indices into `playlist.channels` that pass the current filter.
    visible: Vec<usize>,
    query: String,
    group: Option<String>,
    selected: Option<usize>,
    /// Load problems, shown in the UI.
    notices: Vec<String>,
    /// Created on first Play; dropped when mpv shuts down (window closed).
    player: Option<Player>,
    playing: Option<usize>,
    status: String,
    volume: f64,
    // UI actions are recorded while drawing and applied afterwards.
    pending_play: Option<usize>,
    pending_stop: bool,
    pending_volume: bool,
    pending_save: bool,
    pending_favorite: Option<usize>,
    favorites_only: bool,
    /// EPG matching and now/next lookup; rebuilt whenever playlist or EPG change.
    guide: GuideIndex,
    // Sources and persistence.
    config: Config,
    config_path: Option<PathBuf>,
    playlist_input: String,
    epg_input: String,
    loading: Option<PendingLoad>,
    /// Start loading on the first frame (a repaint handle is needed).
    autoload: bool,
}

impl App {
    /// `args`: optional playlist and EPG sources (they win over saved settings).
    /// With neither args nor saved settings, synthetic demo data is shown.
    pub fn from_args(args: &[String]) -> Self {
        let config_path = config::default_path();
        let config = config_path
            .as_deref()
            .map(Config::load_from)
            .unwrap_or_default();

        let playlist_input = args
            .first()
            .cloned()
            .or_else(|| config.playlist.clone())
            .unwrap_or_default();
        let epg_input = if args.is_empty() {
            config.epg.clone()
        } else {
            args.get(1).cloned()
        }
        .unwrap_or_default();

        let mut app = if playlist_input.is_empty() {
            let (playlist, epg) = demo_data(unix_now());
            Self::new(playlist, epg, vec![])
        } else {
            Self::new(Playlist::default(), Epg::default(), vec![])
        };
        app.volume = config.volume;
        app.autoload = !playlist_input.is_empty();
        app.playlist_input = playlist_input;
        app.epg_input = epg_input;
        app.config = config;
        app.config_path = config_path;
        app
    }

    fn new(playlist: Playlist, epg: Epg, notices: Vec<String>) -> Self {
        let groups = groups(&playlist);
        let visible = filter_channels(&playlist, &Filter::default());
        let guide = GuideIndex::build(&playlist, &epg);
        Self {
            playlist,
            epg,
            groups,
            visible,
            query: String::new(),
            group: None,
            selected: None,
            notices,
            player: None,
            playing: None,
            status: String::new(),
            volume: 80.0,
            pending_play: None,
            pending_stop: false,
            pending_volume: false,
            pending_save: false,
            pending_favorite: None,
            favorites_only: false,
            guide,
            config: Config::default(),
            config_path: None,
            playlist_input: String::new(),
            epg_input: String::new(),
            loading: None,
            autoload: false,
        }
    }

    /// Starts loading the current inputs on a background thread.
    fn start_load(&mut self, ctx: &egui::Context) {
        if self.loading.is_some() {
            return;
        }
        let playlist_src = self.playlist_input.trim().to_string();
        let epg_src = self.epg_input.trim().to_string();
        let (tx, rx) = mpsc::channel();
        let (p, e, ctx) = (playlist_src.clone(), epg_src.clone(), ctx.clone());
        std::thread::spawn(move || {
            let loaded = loader::load_sources(&p, &e);
            // The receiver is gone if the app was closed meanwhile; that's fine.
            let _ = tx.send(loaded);
            ctx.request_repaint(); // wake the UI to pick up the result
        });
        self.notices.clear();
        self.loading = Some(PendingLoad {
            rx,
            playlist_src,
            epg_src,
        });
    }

    fn poll_loading(&mut self) {
        let Some(pending) = &self.loading else {
            return;
        };
        match pending.rx.try_recv() {
            Ok(loaded) => {
                if let Some(done) = self.loading.take() {
                    self.apply_loaded(loaded, &done.playlist_src, &done.epg_src);
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.loading = None;
                self.notices.push("Loading stopped unexpectedly".into());
            }
        }
    }

    fn apply_loaded(&mut self, loaded: Loaded, playlist_src: &str, epg_src: &str) {
        match loaded.playlist {
            Ok(playlist) => {
                self.set_playlist(playlist);
                self.config.playlist = Some(playlist_src.to_string());
                self.config.epg = (!epg_src.is_empty()).then(|| epg_src.to_string());
                self.pending_save = true;
            }
            Err(e) => self.notices.push(format!("Playlist: {e}")),
        }
        match loaded.epg {
            Some(Ok(epg)) => self.epg = epg,
            Some(Err(e)) => self.notices.push(format!("EPG: {e}")),
            None => self.epg = Epg::default(),
        }
        self.rebuild_guide();
    }

    fn rebuild_guide(&mut self) {
        self.guide = GuideIndex::build(&self.playlist, &self.epg);
    }

    fn set_playlist(&mut self, playlist: Playlist) {
        self.groups = groups(&playlist);
        self.playlist = playlist;
        self.group = None;
        // Reselect the channel played last time, if it is still there.
        self.selected = self
            .config
            .last_channel
            .as_deref()
            .and_then(|key| self.playlist.channels.iter().position(|c| c.key() == key));
        self.refilter();
    }

    fn toggle_favorite(&mut self, idx: usize) {
        let Some(channel) = self.playlist.channels.get(idx) else {
            return;
        };
        let key = channel.key().to_string();
        if !self.config.favorites.remove(&key) {
            self.config.favorites.insert(key);
        }
        self.pending_save = true;
        self.refilter();
    }

    fn save_config(&mut self) {
        self.config.volume = self.volume;
        let Some(path) = &self.config_path else {
            return;
        };
        if let Err(e) = self.config.save_to(path) {
            self.notices.push(format!("Cannot save settings: {e}"));
        }
    }

    fn source_bar(&mut self, ui: &mut egui::Ui) {
        let mut load = false;
        ui.horizontal(|ui| {
            ui.label("Playlist");
            load |= source_field(ui, &mut self.playlist_input, "file path or URL");
            ui.label("EPG");
            load |= source_field(ui, &mut self.epg_input, "optional: file path or URL");
            let busy = self.loading.is_some();
            if ui.add_enabled(!busy, egui::Button::new("Load")).clicked() {
                load = true;
            }
            if busy {
                ui.spinner();
            }
        });
        if load {
            self.start_load(ui.ctx());
        }
    }

    /// Makes sure an mpv instance exists, creating one if needed.
    fn ensure_player(&mut self) -> bool {
        if self.player.is_some() {
            return true;
        }
        match Player::new() {
            Ok(player) => {
                self.player = Some(player);
                true
            }
            Err(e) => {
                self.status = format!("Playback unavailable: {e}");
                false
            }
        }
    }

    fn play(&mut self, idx: usize) {
        if !self.ensure_player() {
            return;
        }
        let Some(channel) = self.playlist.channels.get(idx) else {
            return;
        };
        let result = self.player.as_ref().map(|p| p.load(&channel.url));
        match result {
            Some(Ok(())) => {
                self.config.last_channel = Some(channel.key().to_string());
                self.pending_save = true;
                self.playing = Some(idx);
                self.status = "Loading...".into();
                self.apply_volume();
            }
            Some(Err(e)) => self.status = format!("Cannot start playback: {e}"),
            None => {}
        }
    }

    fn stop(&mut self) {
        if let Some(Err(e)) = self.player.as_ref().map(Player::stop) {
            self.status = format!("Cannot stop: {e}");
            return;
        }
        self.playing = None;
        self.status = "Stopped".into();
    }

    fn apply_volume(&mut self) {
        if let Some(Err(e)) = self.player.as_ref().map(|p| p.set_volume(self.volume)) {
            self.status = format!("Cannot set volume: {e}");
        }
    }

    fn poll_player(&mut self) {
        let mut events = Vec::new();
        if let Some(player) = &self.player {
            while let Some(ev) = player.poll() {
                events.push(ev);
            }
        }
        for ev in events {
            self.apply_event(ev);
        }
    }

    fn apply_event(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Loading => self.status = "Loading...".into(),
            PlayerEvent::Started => self.status = "Playing".into(),
            // Also fired when a new channel replaces the old one, so `playing`
            // is left alone; it is cleared by `stop` and by failures.
            PlayerEvent::Ended => self.status = "Stopped".into(),
            PlayerEvent::Shutdown => {
                // Dropping the player destroys mpv and its frozen window;
                // the next Play creates a fresh instance.
                self.player = None;
                self.playing = None;
                self.status = "Video window closed. Press Play to start again.".into();
            }
            PlayerEvent::Failed(msg) => {
                self.playing = None;
                self.status = format!("Playback failed: {msg}");
            }
        }
    }

    fn refilter(&mut self) {
        let filter = Filter {
            query: &self.query,
            group: self.group.as_deref(),
            favorites: self.favorites_only.then_some(&self.config.favorites),
        };
        self.visible = filter_channels(&self.playlist, &filter);
    }

    fn channel_list(&mut self, ui: &mut egui::Ui, now: i64) {
        ui.heading("OpenZap");
        let mut changed = ui.text_edit_singleline(&mut self.query).changed();
        changed |= ui
            .checkbox(&mut self.favorites_only, "Favorites only")
            .changed();

        egui::ComboBox::from_label("Group")
            .selected_text(self.group.as_deref().unwrap_or("All"))
            .show_ui(ui, |ui| {
                if ui.selectable_label(self.group.is_none(), "All").clicked() {
                    self.group = None;
                    changed = true;
                }
                for g in &self.groups {
                    let is_selected = self.group.as_deref() == Some(g);
                    if ui.selectable_label(is_selected, g).clicked() {
                        self.group = Some(g.clone());
                        changed = true;
                    }
                }
            });
        if changed {
            self.refilter();
        }

        ui.label(format!("{} channels", self.visible.len()));
        ui.separator();

        let row_height = ui.text_style_height(&egui::TextStyle::Body);
        let font = egui::TextStyle::Body.resolve(ui.style());
        let (strong, weak) = (ui.visuals().text_color(), ui.visuals().weak_text_color());
        egui::ScrollArea::vertical().show_rows(ui, row_height, self.visible.len(), |ui, range| {
            for &idx in &self.visible[range] {
                let channel = &self.playlist.channels[idx];
                let is_favorite = self.config.favorites.contains(channel.key());
                let on_air = self.guide.now_next(&self.epg, idx, now).now;

                // One line: [* ]Name  ·  current programme (dimmed).
                let mut job = egui::text::LayoutJob::default();
                let prefix = if is_favorite { "* " } else { "" };
                job.append(
                    &format!("{prefix}{}", channel.name),
                    0.0,
                    egui::TextFormat::simple(font.clone(), strong),
                );
                if let Some(p) = on_air {
                    job.append(
                        &format!("   {}", p.title),
                        0.0,
                        egui::TextFormat::simple(font.clone(), weak),
                    );
                }
                let response =
                    ui.add(egui::Button::selectable(self.selected == Some(idx), job).truncate());
                if response.clicked() {
                    self.selected = Some(idx);
                }
                if response.double_clicked() {
                    self.pending_play = Some(idx);
                }
            }
        });
    }

    fn details(&mut self, ui: &mut egui::Ui, now: i64) {
        for notice in &self.notices {
            ui.colored_label(egui::Color32::LIGHT_RED, notice);
        }
        let Some(selected) = self.selected else {
            ui.label("Select a channel on the left.");
            return;
        };
        let Some(channel) = self.playlist.channels.get(selected) else {
            return;
        };

        ui.heading(&channel.name);
        if let Some(group) = &channel.group_title {
            ui.label(format!("Group: {group}"));
        }
        // Never show the raw URL: it may contain an access key.
        ui.monospace(mask_url(&channel.url));

        ui.horizontal(|ui| {
            if ui.button("Play").clicked() {
                self.pending_play = Some(selected);
            }
            if ui.button("Stop").clicked() {
                self.pending_stop = true;
            }
            let label = if self.config.favorites.contains(channel.key()) {
                "Remove from favorites"
            } else {
                "Add to favorites"
            };
            if ui.button(label).clicked() {
                self.pending_favorite = Some(selected);
            }
            let slider = ui.add(egui::Slider::new(&mut self.volume, 0.0..=100.0).text("Volume"));
            if slider.changed() {
                self.pending_volume = true;
            }
            if slider.drag_stopped() {
                self.pending_save = true;
            }
        });
        if !self.status.is_empty() {
            let marker = if self.playing == Some(selected) {
                "* "
            } else {
                ""
            };
            ui.label(format!("{marker}{}", self.status));
        }
        ui.separator();

        let nn = self.guide.now_next(&self.epg, selected, now);
        match nn.now {
            Some(p) => {
                ui.strong(format!("Now: {}", p.title));
                ui.add(
                    egui::ProgressBar::new(progress(p, now))
                        .text(format!("ends in {}", format_duration(p.stop - now))),
                );
                if let Some(desc) = &p.desc {
                    ui.label(desc);
                }
            }
            None => {
                ui.label("Now: no data");
            }
        }
        if let Some(p) = nn.next {
            ui.add_space(8.0);
            ui.label(format!(
                "Next: {} (in {})",
                p.title,
                format_duration(p.start - now)
            ));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = unix_now();
        if std::mem::take(&mut self.autoload) {
            self.start_load(ui.ctx());
        }
        self.poll_loading();
        self.poll_player();
        egui::Panel::top("sources").show(ui, |ui| self.source_bar(ui));
        egui::Panel::left("channels")
            .default_size(320.0)
            .show(ui, |ui| self.channel_list(ui, now));
        egui::CentralPanel::default().show(ui, |ui| self.details(ui, now));
        if let Some(idx) = self.pending_play.take() {
            self.play(idx);
        }
        if let Some(idx) = self.pending_favorite.take() {
            self.toggle_favorite(idx);
        }
        if std::mem::take(&mut self.pending_stop) {
            self.stop();
        }
        if std::mem::take(&mut self.pending_volume) {
            self.apply_volume();
        }
        if std::mem::take(&mut self.pending_save) {
            self.save_config();
        }
        // Poll often while playing or loading; otherwise just keep
        // "ends in ..." fresh without burning CPU.
        let busy = self.playing.is_some() || self.loading.is_some();
        let delay = if busy { 250 } else { 15_000 };
        ui.ctx().request_repaint_after(Duration::from_millis(delay));
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Single-line text field; returns true when Enter was pressed in it.
fn source_field(ui: &mut egui::Ui, text: &mut String, hint: &str) -> bool {
    let response = ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(hint)
            .desired_width(240.0),
    );
    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// Synthetic data (example.com only) so the app is usable without any files.
pub fn demo_data(now: i64) -> (Playlist, Epg) {
    let entries = [
        ("Example News", "news.example", "News"),
        ("Example Film", "film.example", "Movies"),
        ("Example Sport", "sport.example", "Sports"),
    ];
    let mut playlist = Playlist::default();
    let mut epg = Epg::default();
    for (name, id, group) in entries {
        playlist.channels.push(Channel {
            name: name.into(),
            url: format!("http://example.com/{id}.m3u8?token=fakekey"),
            tvg_id: Some(id.into()),
            group_title: Some(group.into()),
            ..Channel::default()
        });
        epg.channels.push(EpgChannel {
            id: id.into(),
            display_name: name.into(),
            icon: None,
        });
        for (i, title) in ["Morning", "Midday", "Evening"].into_iter().enumerate() {
            let start = now - 1800 + i as i64 * 3600;
            epg.programmes.push(Programme {
                channel: id.into(),
                start,
                stop: start + 3600,
                title: format!("{name}: {title}"),
                desc: None,
                category: None,
            });
        }
    }
    (playlist, epg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_data_is_consistent() {
        let now = 1_000_000;
        let (playlist, epg) = demo_data(now);
        assert_eq!(playlist.channels.len(), 3);
        let guide = GuideIndex::build(&playlist, &epg);
        for (i, ch) in playlist.channels.iter().enumerate() {
            let nn = guide.now_next(&epg, i, now);
            assert!(nn.now.is_some(), "{} has no current programme", ch.name);
            assert!(nn.next.is_some(), "{} has no next programme", ch.name);
        }
    }

    #[test]
    fn events_update_status() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.playing = Some(0);

        app.apply_event(PlayerEvent::Started);
        assert_eq!(app.status, "Playing");
        assert_eq!(app.playing, Some(0));

        app.apply_event(PlayerEvent::Failed("loading failed".into()));
        assert_eq!(app.status, "Playback failed: loading failed");
        assert_eq!(app.playing, None);
    }

    #[test]
    fn shutdown_event_drops_player_and_resets_state() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.player = Some(Player::with_options(&[("vo", "null"), ("ao", "null")]).unwrap());
        app.playing = Some(1);

        app.apply_event(PlayerEvent::Shutdown);
        assert!(app.player.is_none());
        assert_eq!(app.playing, None);
        assert!(app.status.contains("closed"));
    }

    fn loaded(playlist: Result<Playlist, String>, epg: Option<Result<Epg, String>>) -> Loaded {
        Loaded { playlist, epg }
    }

    #[test]
    fn successful_load_replaces_data_and_updates_config() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.group = Some("Old".into());
        app.selected = Some(5);

        app.apply_loaded(
            loaded(Ok(playlist), Some(Ok(epg))),
            "http://example.com/l.m3u",
            "http://example.com/e.xml",
        );
        assert_eq!(app.playlist.channels.len(), 3);
        assert_eq!(app.visible.len(), 3);
        assert_eq!(app.groups, ["Movies", "News", "Sports"]);
        assert_eq!((app.group.clone(), app.selected), (None, None));
        assert_eq!(
            app.config.playlist.as_deref(),
            Some("http://example.com/l.m3u")
        );
        assert_eq!(app.config.epg.as_deref(), Some("http://example.com/e.xml"));
        assert!(app.pending_save);
        assert!(app.notices.is_empty());
    }

    #[test]
    fn failed_playlist_keeps_old_data_and_saves_nothing() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);

        app.apply_loaded(
            loaded(Err("server answered HTTP 403".into()), None),
            "x",
            "",
        );
        assert_eq!(app.playlist.channels.len(), 3);
        assert_eq!(app.notices, ["Playlist: server answered HTTP 403"]);
        assert!(!app.pending_save);
        assert_eq!(app.config.playlist, None);
    }

    #[test]
    fn failed_epg_is_reported_but_playlist_loads() {
        let (playlist, _) = demo_data(0);
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);

        app.apply_loaded(
            loaded(Ok(playlist), Some(Err("network error".into()))),
            "p",
            "e",
        );
        assert_eq!(app.playlist.channels.len(), 3);
        assert_eq!(app.notices, ["EPG: network error"]);
        assert!(app.pending_save);
    }

    #[test]
    fn empty_epg_source_clears_epg() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(Playlist::default(), epg, vec![]);

        app.apply_loaded(loaded(Ok(playlist), None), "p", "");
        assert!(app.epg.programmes.is_empty());
        assert_eq!(app.config.epg, None);
    }

    #[test]
    fn background_load_delivers_result() {
        let ctx = egui::Context::default();
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.playlist_input = "/nonexistent/openzap.m3u".into();

        app.start_load(&ctx);
        assert!(app.loading.is_some());
        for _ in 0..200 {
            app.poll_loading();
            if app.loading.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.loading.is_none(), "load did not finish");
        assert_eq!(app.notices.len(), 1);
        assert!(app.notices[0].starts_with("Playlist: cannot read"));
    }

    #[test]
    fn favorites_toggle_filter_and_persist() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);

        app.toggle_favorite(1); // "Example Film"
        assert!(app.config.favorites.contains("film.example"));
        assert!(app.pending_save);

        app.favorites_only = true;
        app.refilter();
        assert_eq!(app.visible, [1]);

        app.toggle_favorite(1); // removes it again
        assert!(app.config.favorites.is_empty());
        assert!(app.visible.is_empty());
    }

    #[test]
    fn last_channel_is_reselected_after_load() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.config.last_channel = Some("sport.example".into());

        app.apply_loaded(loaded(Ok(playlist), Some(Ok(epg))), "p", "e");
        assert_eq!(app.selected, Some(2));
        assert!(app.guide.epg_id(2).is_some());
    }
}
