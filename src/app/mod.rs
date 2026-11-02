//! egui application: channel list beside the video, programme guide and
//! controls in a bar that slides up over the video.

mod list;
mod logo;
mod view;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;
use openzap::browse::{Filter, filter_channels, groups};
use openzap::config::{self, Config, Side, Theme};
use openzap::debuglog::DebugLog;
use openzap::epg::{Epg, EpgChannel, Programme};
use openzap::guide::GuideIndex;
use openzap::loader::{self, Loaded};
use openzap::m3u::{Channel, Playlist};
use openzap::player::{AspectMode, Player, PlayerEvent, PropertyKind, PropertyValue};
use openzap::tracks::{self, AudioTrack};

use crate::settings::{self, Outcome, SettingsDraft};
use crate::video::VideoView;
use list::ListOverlay;
use logo::Logos;
use view::{BAR_START_SECS, BarState};

/// What the current stream is doing, as shown on top of the video.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StreamState {
    Idle,
    Opening,
    Playing,
    /// Waiting for the network; the percentage is how full the cache is.
    Buffering(Option<i64>),
    Failed(String),
}

/// Text to show over the video for a stream state, if any.
fn overlay_text(state: &StreamState) -> Option<String> {
    match state {
        StreamState::Idle | StreamState::Playing => None,
        StreamState::Opening => Some("Loading...".into()),
        StreamState::Buffering(Some(p)) => Some(format!("Buffering... {p}%")),
        StreamState::Buffering(None) => Some("Buffering...".into()),
        StreamState::Failed(msg) => Some(format!("Cannot play: {msg}")),
    }
}

/// Volume step for the mouse wheel, in percent.
const VOLUME_STEP: f64 = 5.0;
/// How long short on-screen messages (volume, mute) stay visible, seconds.
const OSD_SECS: f64 = 1.5;

/// How long stream info stays up after a channel starts, seconds.
const INFO_BRIEF_SECS: f64 = 6.0;
/// How often the audio track list is re-read while the bar is up, seconds.
const TRACKS_REFRESH_SECS: f64 = 1.0;

/// A user command from a button or menu, applied after the frame is drawn
/// (while drawing, `self` is borrowed by the UI code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Stop,
    ToggleMute,
    ToggleFavorite(usize),
    ToggleList,
    SwapListSide,
    ToggleOnTop,
    ToggleFullscreen,
    SetAspect(AspectMode),
    SetAudio(i64),
    OpenSettings,
    Reload,
}

/// How often shown stream info is re-read from mpv (bitrate changes), seconds.
const INFO_REFRESH_SECS: f64 = 1.0;

/// The stream info panel: shown briefly when a stream starts, or pinned
/// with the `I` key. Times are egui input times in seconds.
#[derive(Debug, Default)]
struct InfoOverlay {
    pinned: bool,
    /// A stream started; the brief display begins on the next frame.
    brief_pending: bool,
    brief_until: Option<f64>,
    lines: Vec<String>,
    refreshed_at: Option<f64>,
}

impl InfoOverlay {
    fn toggle(&mut self) {
        self.pinned = !self.pinned;
        self.brief_until = None;
        self.brief_pending = false;
    }

    fn stream_started(&mut self) {
        self.brief_pending = true;
    }

    /// Forgets the previous stream's details.
    fn reset(&mut self) {
        self.lines.clear();
        self.refreshed_at = None;
        self.brief_until = None;
        self.brief_pending = false;
    }

    fn is_visible(&mut self, time: f64) -> bool {
        if std::mem::take(&mut self.brief_pending) {
            self.brief_until = Some(time + INFO_BRIEF_SECS);
        }
        self.pinned || self.brief_until.is_some_and(|until| time < until)
    }

    fn needs_refresh(&self, time: f64) -> bool {
        self.refreshed_at
            .is_none_or(|t| time - t >= INFO_REFRESH_SECS)
    }

    fn set_lines(&mut self, lines: Vec<String>, time: f64) {
        self.lines = lines;
        self.refreshed_at = Some(time);
    }
}

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
    /// Embedded video. Declared before `player`: fields drop in order, so
    /// the renderer goes first (it would keep mpv alive otherwise).
    video: VideoView,
    player: Option<Player>,
    /// Handle for repaint requests from other threads (set by `main`).
    ctx: egui::Context,
    /// Diagnostic log, only with `--debug`.
    log: Option<Arc<DebugLog>>,
    /// The settings dialog, while open.
    settings: Option<SettingsDraft>,
    playing: Option<usize>,
    status: String,
    volume: f64,
    // UI actions are recorded while drawing and applied afterwards.
    pending_play: Option<usize>,
    pending_volume: bool,
    pending_save: bool,
    actions: Vec<Action>,
    stream: StreamState,
    muted: bool,
    /// The bottom bar (guide and controls).
    bar: BarState,
    /// Audio tracks of the current stream.
    tracks: Vec<AudioTrack>,
    tracks_refreshed_at: Option<f64>,
    /// Scroll the list to this channel on the next frame.
    scroll_to: Option<usize>,
    /// The theme still needs to be sent to egui.
    view_pending: bool,
    /// The window is fullscreen (as of the start of this frame).
    fullscreen: bool,
    /// The channel list over the video when it is not docked.
    list_overlay: ListOverlay,
    /// Channel logos; created once the window's context is known.
    logos: Option<Logos>,
    /// Some channel has a logo, so rows reserve room for one.
    show_logos: bool,
    /// The window level last sent (true = always on top).
    on_top_applied: Option<bool>,
    /// Short on-screen message and the input time (seconds) it expires at.
    osd: Option<(String, f64)>,
    info: InfoOverlay,
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
    /// Play the restored last channel once the startup load finishes.
    resume_on_load: bool,
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
        app.resume_on_load = config.resume_last;
        // First start: nothing to show yet, so ask for a playlist right away.
        if playlist_input.is_empty() {
            app.settings = Some(SettingsDraft::new("", &epg_input, &config));
        }
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
        let show_logos = has_logos(&guide, playlist.channels.len());
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
            pending_volume: false,
            pending_save: false,
            actions: Vec::new(),
            stream: StreamState::Idle,
            muted: false,
            bar: BarState::default(),
            tracks: Vec::new(),
            tracks_refreshed_at: None,
            scroll_to: None,
            view_pending: true,
            on_top_applied: None,
            logos: None,
            show_logos,
            fullscreen: false,
            list_overlay: ListOverlay::default(),
            osd: None,
            info: InfoOverlay::default(),
            favorites_only: false,
            guide,
            config: Config::default(),
            config_path: None,
            playlist_input: String::new(),
            epg_input: String::new(),
            loading: None,
            autoload: false,
            resume_on_load: false,
            video: VideoView::default(),
            ctx: egui::Context::default(),
            log: None,
            settings: None,
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
            let loaded = loader::load_sources(&p, &e, unix_now());
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
        self.dlog(&format!(
            "loaded: playlist {}, epg {}",
            match &loaded.playlist {
                Ok(p) => format!("{} channels", p.channels.len()),
                Err(e) => format!("error: {e}"),
            },
            match &loaded.epg {
                None => "none".to_string(),
                Some(Ok(e)) => format!(
                    "{} channels, {} programmes",
                    e.channels.len(),
                    e.programmes.len()
                ),
                Some(Err(e)) => format!("error: {e}"),
            }
        ));
        match loaded.playlist {
            Ok(playlist) => {
                self.set_playlist(playlist);
                if std::mem::take(&mut self.resume_on_load) {
                    self.pending_play = self.selected;
                }
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
        self.show_logos = has_logos(&self.guide, self.playlist.channels.len());
    }

    /// Texture of the channel's logo, if it has one and it is loaded.
    fn logo_texture(&self, idx: usize) -> Option<egui::TextureHandle> {
        let url = self.guide.logo(idx)?;
        self.logos.as_ref()?.texture(&self.ctx, url)
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

    /// Takes over the settings from the dialog. Returns true if the sources
    /// changed and must be loaded again.
    fn apply_settings(&mut self, draft: SettingsDraft) -> bool {
        let playlist = draft.playlist.trim().to_string();
        let epg = draft.epg.trim().to_string();
        let sources_changed =
            playlist != self.playlist_input.trim() || epg != self.epg_input.trim();
        self.config.playlist = (!playlist.is_empty()).then(|| playlist.clone());
        self.config.epg = (!epg.is_empty()).then(|| epg.clone());
        self.playlist_input = playlist;
        self.epg_input = epg;
        self.config.resume_last = draft.resume_last;
        if draft.theme != self.config.view.theme {
            self.config.view.theme = draft.theme;
            self.view_pending = true;
        }
        if draft.playback != self.config.playback {
            self.config.playback = draft.playback;
            let applied = self.player.as_ref().map(|p| p.apply(&draft.playback));
            if let Some(Err(e)) = applied {
                self.status = format!("Cannot apply playback settings: {e}");
            }
        }
        self.pending_save = true;
        sources_changed && !self.playlist_input.is_empty()
    }

    pub fn set_context(&mut self, ctx: egui::Context) {
        self.logos = Some(Logos::new(&ctx));
        self.ctx = ctx;
    }

    /// Enables the diagnostic log (`--debug`).
    pub fn set_debug_log(&mut self, log: Arc<DebugLog>) {
        self.video.enable_diagnostics();
        self.log = Some(log);
    }

    /// Writes to the debug log if enabled (URLs are masked by the log).
    fn dlog(&self, msg: &str) {
        if let Some(log) = &self.log {
            log.line(msg);
        }
    }

    /// Makes sure an mpv instance with an attached video view exists.
    ///
    /// Runs inside `ui`, where eframe keeps the GL context current, which
    /// creating the renderer requires.
    fn ensure_player(&mut self) -> bool {
        if self.player.is_some() {
            return true;
        }
        let player = match Player::new(&self.config.playback) {
            Ok(player) => player,
            Err(e) => {
                self.dlog(&format!("player creation failed: {e}"));
                self.status = format!("Playback unavailable: {e}");
                return false;
            }
        };
        if let Err(e) = self.video.attach(&player, &self.ctx) {
            self.dlog(&format!("video attach failed: {e}"));
            self.status = format!("Video output unavailable: {e}");
            return false;
        }
        if self.log.is_some() {
            if let Err(e) = player.request_log_messages("v") {
                self.dlog(&format!("cannot get mpv log: {e}"));
            }
            self.dlog("player created, video attached");
        }
        // Buffering state for the overlay; without it playback still works.
        for (name, kind) in [
            ("paused-for-cache", PropertyKind::Flag),
            ("cache-buffering-state", PropertyKind::Int),
        ] {
            if let Err(e) = player.observe(name, kind) {
                self.notices.push(format!("Cannot watch {name}: {e}"));
            }
        }
        if let Err(e) = player.set_aspect(self.config.view.aspect) {
            self.dlog(&format!("cannot set picture size: {e}"));
        }
        self.player = Some(player);
        true
    }

    /// Releases the renderer and then mpv (GL context must be current).
    fn release_player(&mut self) {
        self.video.detach();
        self.player = None;
        self.playing = None;
    }

    fn play(&mut self, idx: usize) {
        if !self.ensure_player() {
            return;
        }
        let Some(channel) = self.playlist.channels.get(idx) else {
            return;
        };
        self.dlog(&format!("play channel #{idx}: {}", channel.name));
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
        self.stream = StreamState::Idle;
        self.status = "Stopped".into();
    }

    /// Plays the next (`delta` = 1) or previous (-1) channel of the visible
    /// list, wrapping around. Starts from the playing or selected channel.
    fn switch_channel(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let current = self.playing.or(self.selected);
        let pos = current.and_then(|c| self.visible.iter().position(|&i| i == c));
        let next = match pos {
            Some(p) => (p as isize + delta).rem_euclid(self.visible.len() as isize) as usize,
            None => 0,
        };
        let idx = self.visible[next];
        self.selected = Some(idx);
        self.pending_play = Some(idx);
        self.scroll_to = Some(idx);
    }

    fn toggle_mute(&mut self, now: f64) {
        self.muted = !self.muted;
        self.apply_volume();
        let text = if self.muted { "Muted" } else { "Sound on" };
        self.show_osd(text, now);
    }

    fn change_volume(&mut self, delta: f64, now: f64) {
        self.volume = (self.volume + delta).clamp(0.0, 100.0);
        self.pending_volume = true;
        self.pending_save = true;
        self.osd = Some((format!("Volume {:.0}%", self.volume), now + OSD_SECS));
    }

    /// Sends volume and mute state to mpv.
    fn apply_volume(&mut self) {
        let result = self.player.as_ref().map(|p| {
            p.set_volume(self.volume)
                .and_then(|()| p.set_mute(self.muted))
        });
        if let Some(Err(e)) = result {
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
            match &ev {
                PlayerEvent::Log {
                    prefix,
                    level,
                    text,
                } => self.dlog(&format!("mpv [{prefix}] {level}: {text}")),
                other => self.dlog(&format!("player event: {other:?}")),
            }
            self.apply_event(ev);
        }
        for report in self.video.take_reports() {
            self.dlog(&report);
        }
    }

    fn apply_event(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Loading => {
                self.video.clear();
                self.info.reset();
                self.tracks.clear();
                self.tracks_refreshed_at = None;
                self.stream = StreamState::Opening;
                self.status = "Loading...".into();
            }
            PlayerEvent::Started => {
                self.info.stream_started();
                self.bar.request(BAR_START_SECS);
                self.tracks_refreshed_at = None;
                self.stream = StreamState::Playing;
                self.status = "Playing".into();
            }
            PlayerEvent::Property { name, value } => self.apply_property(&name, value),
            PlayerEvent::Log { .. } => {} // only written to the debug log
            // Also fired when a new channel replaces the old one, so `playing`
            // is left alone; it is cleared by `stop` and by failures.
            PlayerEvent::Ended => {
                self.stream = StreamState::Idle;
                self.status = "Stopped".into();
            }
            PlayerEvent::Shutdown => {
                // The next Play creates a fresh instance.
                self.release_player();
                self.stream = StreamState::Idle;
                self.status = "Player closed. Press Play to start again.".into();
            }
            PlayerEvent::Failed(msg) => {
                self.info.reset();
                self.stream = StreamState::Failed(msg.clone());
                self.playing = None;
                self.status = format!("Playback failed: {msg}");
            }
        }
    }

    fn apply_property(&mut self, name: &str, value: PropertyValue) {
        match (name, value) {
            ("paused-for-cache", PropertyValue::Flag(true)) => {
                if matches!(self.stream, StreamState::Playing | StreamState::Opening) {
                    self.stream = StreamState::Buffering(None);
                }
            }
            ("paused-for-cache", PropertyValue::Flag(false)) => {
                if matches!(self.stream, StreamState::Buffering(_)) {
                    self.stream = StreamState::Playing;
                }
            }
            ("cache-buffering-state", PropertyValue::Int(p)) => {
                if let StreamState::Buffering(percent) = &mut self.stream {
                    *percent = Some(p.clamp(0, 100));
                }
            }
            _ => {}
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

    /// Global keyboard shortcuts (ignored while typing in a text field).
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.settings.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let pressed = |key| ctx.input(|i| i.key_pressed(key));
        let time = ctx.input(|i| i.time);
        if pressed(egui::Key::F) {
            self.set_fullscreen(ctx, !is_fullscreen(ctx));
        }
        if pressed(egui::Key::Escape) && is_fullscreen(ctx) {
            self.set_fullscreen(ctx, false);
        }
        if pressed(egui::Key::M) {
            self.toggle_mute(time);
        }
        if pressed(egui::Key::I) {
            self.info.toggle();
        }
        if pressed(egui::Key::L) {
            self.toggle_list(time);
        }
        if pressed(egui::Key::T) {
            self.toggle_on_top(time);
        }
        if pressed(egui::Key::Z) {
            self.set_aspect(self.config.view.aspect.next(), time);
        }
        if pressed(egui::Key::A) {
            self.next_audio_track(time);
        }
        if pressed(egui::Key::Enter)
            && let Some(idx) = self.selected
        {
            self.pending_play = Some(idx);
        }
        if pressed(egui::Key::PageUp) {
            self.switch_channel(-1);
        }
        if pressed(egui::Key::PageDown) {
            self.switch_channel(1);
        }
    }

    /// Applies the actions recorded while drawing.
    fn apply_actions(&mut self, ctx: &egui::Context) {
        let time = ctx.input(|i| i.time);
        for action in std::mem::take(&mut self.actions) {
            match action {
                Action::Stop => self.stop(),
                Action::ToggleMute => self.toggle_mute(time),
                Action::ToggleFavorite(idx) => self.toggle_favorite(idx),
                Action::ToggleList => self.toggle_list(time),
                Action::SwapListSide => {
                    self.config.view.list_side = match self.config.view.list_side {
                        Side::Left => Side::Right,
                        Side::Right => Side::Left,
                    };
                    self.pending_save = true;
                }
                Action::ToggleOnTop => self.toggle_on_top(time),
                Action::ToggleFullscreen => self.set_fullscreen(ctx, !is_fullscreen(ctx)),
                Action::SetAspect(mode) => self.set_aspect(mode, time),
                Action::SetAudio(id) => self.select_audio_track(id, time),
                Action::OpenSettings => {
                    self.settings = Some(SettingsDraft::new(
                        &self.playlist_input,
                        &self.epg_input,
                        &self.config,
                    ));
                }
                Action::Reload => self.start_load(ctx),
            }
        }
    }

    fn is_favorite(&self, idx: usize) -> bool {
        self.playlist
            .channels
            .get(idx)
            .is_some_and(|c| self.config.favorites.contains(c.key()))
    }

    fn show_osd(&mut self, text: impl Into<String>, time: f64) {
        self.osd = Some((text.into(), time + OSD_SECS));
    }

    /// Windowed: docks or hides the list. Fullscreen (or with the list
    /// hidden): pins or unpins the list over the video.
    fn toggle_list(&mut self, time: f64) {
        if self.fullscreen {
            self.list_overlay.pinned = !self.list_overlay.pinned;
            return;
        }
        self.list_overlay.pinned = false;
        self.config.view.list_visible = !self.config.view.list_visible;
        self.pending_save = true;
        if !self.config.view.list_visible {
            self.show_osd("List hidden (L to show)", time);
        }
    }

    fn toggle_on_top(&mut self, time: f64) {
        self.config.view.always_on_top = !self.config.view.always_on_top;
        self.pending_save = true;
        let text = if self.config.view.always_on_top {
            "Always on top"
        } else {
            "Normal window"
        };
        self.show_osd(text, time);
    }

    fn set_aspect(&mut self, mode: AspectMode, time: f64) {
        self.config.view.aspect = mode;
        self.pending_save = true;
        if let Some(Err(e)) = self.player.as_ref().map(|p| p.set_aspect(mode)) {
            self.status = format!("Cannot change picture size: {e}");
        }
        self.show_osd(format!("Picture: {}", mode.label()), time);
    }

    /// Re-reads the audio tracks from mpv, at most once per interval.
    fn refresh_tracks(&mut self, time: f64) {
        let due = self
            .tracks_refreshed_at
            .is_none_or(|t| time - t >= TRACKS_REFRESH_SECS);
        if !due {
            return;
        }
        if let Some(player) = &self.player {
            self.tracks = tracks::audio_tracks(|name| player.get_property(name));
        }
        self.tracks_refreshed_at = Some(time);
    }

    fn next_audio_track(&mut self, time: f64) {
        self.tracks_refreshed_at = None;
        self.refresh_tracks(time);
        match tracks::next_track(&self.tracks).map(|t| t.id) {
            Some(id) => self.select_audio_track(id, time),
            None => self.show_osd("No other audio tracks", time),
        }
    }

    fn select_audio_track(&mut self, id: i64, time: f64) {
        if let Some(Err(e)) = self.player.as_ref().map(|p| p.set_audio_track(id)) {
            self.status = format!("Cannot switch audio: {e}");
            return;
        }
        for t in &mut self.tracks {
            t.selected = t.id == id;
        }
        let label = self
            .tracks
            .iter()
            .find(|t| t.id == id)
            .map_or_else(|| format!("Track {id}"), |t| t.label());
        self.show_osd(format!("Audio: {label}"), time);
    }

    /// Enters or leaves fullscreen. A fullscreen window is on top anyway;
    /// keeping it "always on top" as well makes Windows treat it as an
    /// exclusive fullscreen app and switch the display mode (the screen
    /// goes black for seconds). So the level is dropped first.
    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        if on {
            self.send_on_top(ctx, false);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        // Leaving: `apply_view` restores the level once the window is back.
    }

    fn send_on_top(&mut self, ctx: &egui::Context, on_top: bool) {
        if self.on_top_applied == Some(on_top) {
            return;
        }
        self.on_top_applied = Some(on_top);
        let level = if on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
    }

    /// Sends the window level and theme to the window when they change.
    fn apply_view(&mut self, ctx: &egui::Context) {
        let on_top = wants_on_top(self.config.view.always_on_top, is_fullscreen(ctx));
        self.send_on_top(ctx, on_top);
        if !std::mem::take(&mut self.view_pending) {
            return;
        }
        ctx.set_theme(match self.config.view.theme {
            Theme::System => egui::ThemePreference::System,
            Theme::Dark => egui::ThemePreference::Dark,
            Theme::Light => egui::ThemePreference::Light,
        });
    }
}

impl eframe::App for App {
    /// The GL context is still alive here, so mpv's renderer can be freed
    /// properly before the window goes away.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.release_player();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = unix_now();
        if std::mem::take(&mut self.autoload) {
            self.start_load(ui.ctx());
        }
        self.poll_loading();
        self.poll_player();
        self.handle_shortcuts(ui.ctx());
        self.apply_view(ui.ctx());
        let fullscreen = is_fullscreen(ui.ctx());
        self.fullscreen = fullscreen;
        let docked = !fullscreen && self.config.view.list_visible;
        let screen = ui.max_rect();
        if docked {
            let panel = match self.config.view.list_side {
                Side::Left => egui::Panel::left("channels"),
                Side::Right => egui::Panel::right("channels"),
            };
            panel
                .default_size(340.0)
                .min_size(240.0)
                .show(ui, |ui| self.channel_panel(ui, now));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
            .show(ui, |ui| self.video_area(ui, fullscreen, now));
        if !docked {
            self.list_overlay(ui.ctx(), screen, now);
        }
        let ctx = ui.ctx().clone();
        if let Some(draft) = &mut self.settings
            && let Some(outcome) = settings::show(&ctx, draft)
        {
            let draft = self.settings.take();
            if let (Outcome::Save, Some(draft)) = (outcome, draft)
                && self.apply_settings(draft)
            {
                self.start_load(&ctx);
            }
        }
        if let Some(idx) = self.pending_play.take() {
            self.play(idx);
        }
        self.apply_actions(&ctx);
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

fn is_fullscreen(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
}

fn has_logos(guide: &GuideIndex, channels: usize) -> bool {
    (0..channels).any(|i| guide.logo(i).is_some())
}

/// Whether the window should be "always on top": never in fullscreen
/// (see `App::set_fullscreen`).
fn wants_on_top(setting: bool, fullscreen: bool) -> bool {
    setting && !fullscreen
}

/// Text on a dark rounded background, anchored at `pos`.
fn draw_label(
    painter: &egui::Painter,
    pos: egui::Pos2,
    anchor: egui::Align2,
    text: &str,
    style: egui::TextStyle,
    ui: &egui::Ui,
) {
    let font = style.resolve(ui.style());
    let galley = painter.layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.rect_filled(rect.expand(10.0), 6.0, egui::Color32::from_black_alpha(170));
    painter.galley(rect.min, galley, egui::Color32::WHITE);
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
        assert!(!app.video.is_attached());
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
    fn resume_plays_restored_channel_only_on_startup_load() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.config.last_channel = Some("film.example".into());
        app.resume_on_load = true;

        app.apply_loaded(loaded(Ok(playlist.clone()), None), "p", "");
        assert_eq!(app.pending_play, Some(1));

        // A later manual reload must not start playback by itself.
        app.pending_play = None;
        app.apply_loaded(loaded(Ok(playlist), Some(Ok(epg))), "p", "e");
        assert_eq!(app.pending_play, None);
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

    #[test]
    fn overlay_texts() {
        assert_eq!(overlay_text(&StreamState::Idle), None);
        assert_eq!(overlay_text(&StreamState::Playing), None);
        assert_eq!(
            overlay_text(&StreamState::Opening).as_deref(),
            Some("Loading...")
        );
        assert_eq!(
            overlay_text(&StreamState::Buffering(Some(40))).as_deref(),
            Some("Buffering... 40%")
        );
        assert_eq!(
            overlay_text(&StreamState::Failed("loading failed".into())).as_deref(),
            Some("Cannot play: loading failed")
        );
    }

    fn prop(name: &str, value: PropertyValue) -> PlayerEvent {
        PlayerEvent::Property {
            name: name.into(),
            value,
        }
    }

    #[test]
    fn stream_state_follows_player_events() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);

        app.apply_event(PlayerEvent::Loading);
        assert_eq!(app.stream, StreamState::Opening);
        assert!(app.video.is_clear_pending(), "old picture must be blanked");
        app.apply_event(PlayerEvent::Started);
        assert_eq!(app.stream, StreamState::Playing);

        app.apply_event(prop("paused-for-cache", PropertyValue::Flag(true)));
        assert_eq!(app.stream, StreamState::Buffering(None));
        app.apply_event(prop("cache-buffering-state", PropertyValue::Int(40)));
        assert_eq!(app.stream, StreamState::Buffering(Some(40)));
        app.apply_event(prop("paused-for-cache", PropertyValue::Flag(false)));
        assert_eq!(app.stream, StreamState::Playing);

        app.apply_event(PlayerEvent::Failed("loading failed".into()));
        assert_eq!(app.stream, StreamState::Failed("loading failed".into()));
    }

    #[test]
    fn buffering_is_ignored_when_nothing_plays() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.apply_event(prop("paused-for-cache", PropertyValue::Flag(true)));
        app.apply_event(prop("cache-buffering-state", PropertyValue::Int(10)));
        assert_eq!(app.stream, StreamState::Idle);
    }

    #[test]
    fn switch_channel_wraps_within_visible_list() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);

        app.switch_channel(1); // nothing selected: start at the first
        assert_eq!(app.pending_play, Some(0));

        app.playing = Some(2);
        app.switch_channel(1); // last -> first
        assert_eq!((app.selected, app.pending_play), (Some(0), Some(0)));

        app.playing = Some(0);
        app.switch_channel(-1); // first -> last
        assert_eq!(app.pending_play, Some(2));
    }

    #[test]
    fn switch_channel_respects_filter() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.query = "film".into();
        app.refilter(); // only index 1 visible
        app.playing = Some(1);
        app.switch_channel(1);
        assert_eq!(app.pending_play, Some(1));

        app.visible.clear();
        app.pending_play = None;
        app.switch_channel(1);
        assert_eq!(app.pending_play, None);
    }

    #[test]
    fn stream_info_shows_briefly_on_start_and_toggles() {
        let mut info = InfoOverlay::default();
        assert!(!info.is_visible(0.0));

        info.stream_started();
        assert!(info.is_visible(10.0));
        assert!(info.is_visible(10.0 + INFO_BRIEF_SECS - 0.1));
        assert!(!info.is_visible(10.0 + INFO_BRIEF_SECS));

        info.toggle();
        assert!(info.is_visible(100.0));
        // Pinned info survives channel changes.
        info.reset();
        info.stream_started();
        assert!(info.is_visible(1000.0));
        info.toggle();
        assert!(!info.is_visible(1000.0));
    }

    #[test]
    fn stream_info_refreshes_once_per_interval() {
        let mut info = InfoOverlay::default();
        assert!(info.needs_refresh(5.0));
        info.set_lines(vec!["Video: 1920×1080".into()], 5.0);
        assert!(!info.needs_refresh(5.5));
        assert!(info.needs_refresh(5.0 + INFO_REFRESH_SECS));
        info.reset();
        assert!(info.lines.is_empty() && info.needs_refresh(5.1));
    }

    #[test]
    fn player_events_reset_and_start_stream_info() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, Vec::new());
        app.info.set_lines(vec!["old".into()], 0.0);
        app.apply_event(PlayerEvent::Loading);
        assert!(app.info.lines.is_empty());
        app.apply_event(PlayerEvent::Started);
        assert!(app.info.is_visible(1.0));
    }

    #[test]
    fn view_actions_change_and_persist_settings() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        let ctx = egui::Context::default();
        app.view_pending = false;

        app.actions = vec![
            Action::ToggleList,
            Action::SwapListSide,
            Action::ToggleOnTop,
            Action::SetAspect(AspectMode::Fill),
        ];
        app.apply_actions(&ctx);
        let view = app.config.view;
        assert!(!view.list_visible);
        assert_eq!(view.list_side, Side::Right);
        assert!(view.always_on_top);
        assert_eq!(view.aspect, AspectMode::Fill);
        assert!(app.pending_save && app.actions.is_empty());
        assert_eq!(
            app.osd.map(|(t, _)| t).as_deref(),
            Some("Picture: Fill (crop)")
        );
    }

    #[test]
    fn fullscreen_window_is_never_always_on_top() {
        assert!(wants_on_top(true, false));
        assert!(!wants_on_top(true, true));
        assert!(!wants_on_top(false, false));
    }

    #[test]
    fn window_level_is_sent_only_on_change() {
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        let ctx = egui::Context::default();
        app.config.view.always_on_top = true;
        app.apply_view(&ctx);
        assert_eq!(app.on_top_applied, Some(true));
        // Going fullscreen drops the level right away.
        app.set_fullscreen(&ctx, true);
        assert_eq!(app.on_top_applied, Some(false));
    }

    #[test]
    fn list_key_pins_the_overlay_in_fullscreen_and_docks_otherwise() {
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.fullscreen = true;
        app.toggle_list(0.0);
        assert!(app.list_overlay.pinned);
        assert!(app.config.view.list_visible, "docking is left alone");
        app.toggle_list(0.0);
        assert!(!app.list_overlay.pinned);

        app.fullscreen = false;
        app.toggle_list(0.0);
        assert!(!app.config.view.list_visible);
        assert!(app.pending_save);
    }

    #[test]
    fn favorite_action_and_lookup() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.actions.push(Action::ToggleFavorite(2));
        app.apply_actions(&egui::Context::default());
        assert!(app.is_favorite(2) && !app.is_favorite(0) && !app.is_favorite(99));
    }

    #[test]
    fn audio_cycling_without_tracks_says_so() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.next_audio_track(1.0);
        assert_eq!(
            app.osd.map(|(t, _)| t).as_deref(),
            Some("No other audio tracks")
        );
    }

    #[test]
    fn channel_start_brings_up_the_bar_and_rereads_tracks() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.tracks_refreshed_at = Some(0.0);
        app.apply_event(PlayerEvent::Started);
        assert!(app.tracks_refreshed_at.is_none());
        assert!(app.bar.is_visible(100.0, false));
    }

    #[test]
    fn keyboard_switch_scrolls_the_list() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.playing = Some(0);
        app.switch_channel(1);
        assert_eq!(app.scroll_to, Some(1));
    }

    #[test]
    fn theme_change_is_saved_and_applied() {
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.view_pending = false;
        let mut d = draft(&app);
        d.theme = Theme::Dark;
        app.apply_settings(d);
        assert_eq!(app.config.view.theme, Theme::Dark);
        assert!(app.view_pending && app.pending_save);
    }

    #[test]
    fn mute_and_volume_update_state_and_osd() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);

        app.toggle_mute(10.0);
        assert!(app.muted);
        assert_eq!(app.osd, Some(("Muted".into(), 10.0 + OSD_SECS)));

        app.volume = 98.0;
        app.change_volume(VOLUME_STEP, 20.0);
        assert_eq!(app.volume, 100.0);
        assert!(app.pending_volume && app.pending_save);
        app.change_volume(-200.0, 20.0);
        assert_eq!(app.volume, 0.0);
    }

    fn draft(app: &App) -> SettingsDraft {
        SettingsDraft::new(&app.playlist_input, &app.epg_input, &app.config)
    }

    #[test]
    fn saving_new_sources_requests_reload_and_persists() {
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        let mut d = draft(&app);
        d.playlist = "  http://example.com/list.m3u  ".into();
        d.resume_last = true;

        assert!(app.apply_settings(d));
        assert_eq!(app.playlist_input, "http://example.com/list.m3u");
        assert_eq!(
            app.config.playlist.as_deref(),
            Some("http://example.com/list.m3u")
        );
        assert_eq!(app.config.epg, None);
        assert!(app.config.resume_last && app.pending_save);
    }

    #[test]
    fn saving_unchanged_sources_does_not_reload() {
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.playlist_input = "/lists/a.m3u".into();
        let mut d = draft(&app);
        d.playback.buffer_secs = 42;

        assert!(!app.apply_settings(d));
        assert_eq!(app.config.playback.buffer_secs, 42);
    }

    #[test]
    fn clearing_the_playlist_does_not_reload() {
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.playlist_input = "/lists/a.m3u".into();
        let mut d = draft(&app);
        d.playlist.clear();
        assert!(!app.apply_settings(d));
        assert_eq!(app.config.playlist, None);
    }
}
