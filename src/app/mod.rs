//! egui application: channel list beside the video, programme guide and
//! controls in a bar that slides up over the video.

mod list;
mod logo;
mod palette;
mod schedule;
mod view;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;
use openzap::browse::{Filter, channel_numbers, filter_channels, find_number, groups};
use openzap::catchup::Catchup;
use openzap::config::{self, Config, Side, Theme};
use openzap::debuglog::DebugLog;
use openzap::diagnose::{self, ErrorBurst};
use openzap::epg::{Epg, EpgChannel, Programme};
use openzap::guide::GuideIndex;
use openzap::loader::{self, Fetch, Fetched, LoadEvent};
use openzap::m3u::{Channel, Playlist};
use openzap::platform;
use openzap::player::{AspectMode, Player, PlayerEvent, PropertyKind, PropertyValue};
use openzap::timeshift::{self, Position, Target};
use openzap::tracks::{self, Track};
use openzap::{cache, catchup, clock};

use crate::filepick::{self, Pick, PickState, PickTarget};
use crate::settings::{self, Outcome, SettingsDraft};
use crate::video::VideoView;
use list::ListOverlay;
use logo::Logos;
use schedule::ScheduleView;
use view::{BAR_START_SECS, BarState};

/// Which part of the window the arrow keys and Enter act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Focus {
    /// ↑/↓ switch channels, ←/→ move in the archive, Enter opens the list.
    #[default]
    Video,
    /// ↑/↓ move the selection, ←/→ change the group, Enter plays.
    List,
    /// ↑/↓ move through the programmes, Enter opens one (again: Watch).
    Guide,
}

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
/// Messages about a problem stay up longer than volume and the like.
const AUDIO_OFF_OSD_SECS: f64 = 6.0;

/// How long stream info stays up after a channel starts, seconds.
const INFO_BRIEF_SECS: f64 = 6.0;
/// Shown instead of toggling "always on top" where the app cannot do it.
const ON_TOP_UNAVAILABLE: &str = "Wayland: use the window menu (Alt+Space) for Always on Top";

/// How often the audio track list is re-read while the bar is up, seconds.
const TRACKS_REFRESH_SECS: f64 = 1.0;

/// Finds a programme boundary from a moment (see [`timeshift`]).
type Find = fn(&[&Programme], i64) -> Option<i64>;

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
    ToggleSchedule,
    Restart,
    ToggleFullscreen,
    SetAspect(AspectMode),
    SetAudio(i64),
    /// `None` turns subtitles off.
    SetSubtitle(Option<i64>),
    OpenSettings,
    Reload,
    /// Plays `channel` from the archive at the moment `start` (Unix seconds).
    PlayArchive {
        channel: usize,
        start: i64,
    },
    /// From the archive back to the live stream.
    BackToLive,
    /// Moves the channel playing by this many seconds (timeshift).
    Seek(i64),
    /// Moves the channel playing to this moment (Unix seconds).
    SeekTo(i64),
    /// To the start of the programme (or the one before, if it just began).
    ProgrammeStart,
    NextProgramme,
    TogglePause,
    /// Sets the guide time shift of `channel` (minutes).
    SetShift {
        channel: usize,
        minutes: i64,
    },
}

/// How often shown stream info is re-read from mpv (bitrate changes), seconds.
const INFO_REFRESH_SECS: f64 = 1.0;

/// What the `I` key shows over the video; it cycles Off → Programme →
/// Stream → Off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum InfoMode {
    #[default]
    Off,
    /// The programme in full: title, time, whole description, next.
    Programme,
    /// Technical details of the stream.
    Stream,
}

impl InfoMode {
    fn next(self) -> Self {
        match self {
            InfoMode::Off => InfoMode::Programme,
            InfoMode::Programme => InfoMode::Stream,
            InfoMode::Stream => InfoMode::Off,
        }
    }
}

/// The info panel: the one chosen with `I`, or else the stream info for a
/// moment when a stream starts. Times are egui input times in seconds.
#[derive(Debug, Default)]
struct InfoOverlay {
    mode: InfoMode,
    /// A stream started; the brief display begins on the next frame.
    brief_pending: bool,
    brief_until: Option<f64>,
    lines: Vec<String>,
    refreshed_at: Option<f64>,
}

impl InfoOverlay {
    fn cycle(&mut self) {
        self.mode = self.mode.next();
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

    /// The panel to show now (`Off`: none).
    fn shown(&mut self, time: f64) -> InfoMode {
        if std::mem::take(&mut self.brief_pending) {
            self.brief_until = Some(time + INFO_BRIEF_SECS);
        }
        match self.mode {
            InfoMode::Off if self.brief_until.is_some_and(|until| time < until) => InfoMode::Stream,
            mode => mode,
        }
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

/// A stream that shows no picture for this long is given up, seconds.
const STALL_SECS: f64 = 20.0;
/// Why a stream was given up (shown as "Cannot play: ...").
const STALL_MESSAGE: &str = "no picture for 20 s, the channel may be offline. Press R to retry";

/// Notices a stream that keeps waiting (opening, buffering, or open but
/// without a single frame). Times are egui input times in seconds.
#[derive(Debug, Default)]
struct StallWatch {
    since: Option<f64>,
    last_check: Option<f64>,
}

/// A longer gap between checks means the window was not drawing (e.g.
/// minimized), so no frames could be counted: the wait starts over.
const STALL_CHECK_GAP: f64 = 2.0;

impl StallWatch {
    /// `waiting`: the stream shows no picture right now. True once it has
    /// waited `STALL_SECS` without a break.
    fn timed_out(&mut self, time: f64, waiting: bool) -> bool {
        if self
            .last_check
            .replace(time)
            .is_some_and(|last| time - last > STALL_CHECK_GAP)
        {
            self.since = None;
        }
        if !waiting {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(time);
        if time - since >= STALL_SECS {
            self.since = None;
            return true;
        }
        false
    }
}

/// Typed digits are taken as a channel number after this pause, seconds.
const DIGIT_PAUSE_SECS: f64 = 1.5;
const MAX_DIGITS: usize = 5;

/// Channel number typed on the keyboard. Times are egui input times.
#[derive(Debug, Default)]
struct DigitEntry {
    digits: String,
    last: f64,
}

impl DigitEntry {
    fn push(&mut self, digit: char, time: f64) {
        if self.digits.len() < MAX_DIGITS {
            self.digits.push(digit);
        }
        self.last = time;
    }

    /// Removes the last digit; false if there was nothing to remove.
    fn back(&mut self, time: f64) -> bool {
        self.last = time;
        self.digits.pop().is_some()
    }

    fn cancel(&mut self) -> bool {
        !std::mem::take(&mut self.digits).is_empty()
    }

    fn text(&self) -> Option<&str> {
        (!self.digits.is_empty()).then_some(self.digits.as_str())
    }

    /// The number, once confirmed (Enter) or after a pause in typing.
    fn finish(&mut self, time: f64, confirm: bool) -> Option<u32> {
        if self.digits.is_empty() || !(confirm || time - self.last >= DIGIT_PAUSE_SECS) {
            return None;
        }
        std::mem::take(&mut self.digits).parse().ok()
    }

    /// Seconds until the typed number is taken, while typing.
    fn remaining(&self, time: f64) -> Option<f64> {
        self.text()
            .map(|_| (self.last + DIGIT_PAUSE_SECS - time).max(0.0))
    }
}

/// After a failed update while running, the next try waits this long, seconds.
const UPDATE_RETRY_SECS: i64 = 3600;

/// A background load in progress, with the inputs it was started for.
struct PendingLoad {
    rx: Receiver<LoadEvent>,
    /// Set to stop the load (a newer one replaces it).
    cancel: Arc<AtomicBool>,
    playlist_src: String,
    epg_src: String,
    /// Still to come.
    playlist_due: bool,
    epg_due: bool,
}

impl Drop for PendingLoad {
    /// A replaced or abandoned load stops downloading.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
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
    /// A file dialog opened from the settings, while open.
    file_pick: Option<Pick>,
    playing: Option<usize>,
    /// Set while `playing` (or the stream last started) is from the archive.
    archive: Option<Position>,
    /// The moment on screen (Unix seconds): updated every frame; "now" for
    /// live streams.
    shown_at: i64,
    /// Paused: since when (input time) and at which moment.
    paused: Option<(f64, i64)>,
    /// The archive setting changed: the guide is loaded again for its history.
    reload_epg: bool,
    /// Recent audio decoding errors of the current stream.
    audio_errors: ErrorBurst,
    /// Sound was turned off because it could not be decoded.
    audio_disabled: bool,
    /// The HTTP status of the last failed request of the current stream.
    http_error: Option<u16>,
    /// mpv's reason for the last failure, kept to add details arriving later.
    failure: Option<String>,
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
    tracks: Vec<Track>,
    /// Subtitle tracks of the current stream.
    subtitles: Vec<Track>,
    /// `sub-pos` last sent to mpv (subtitles are lifted above the bar).
    sub_pos_applied: Option<u32>,
    tracks_refreshed_at: Option<f64>,
    /// Scroll the list to this channel on the next frame.
    scroll_to: Option<usize>,
    /// The channel list was on screen last frame.
    list_was_shown: bool,
    /// The theme still needs to be sent to egui.
    view_pending: bool,
    /// The window is fullscreen (as of the start of this frame).
    fullscreen: bool,
    /// The channel list over the video when it is not docked.
    list_overlay: ListOverlay,
    /// The programme guide panel.
    schedule: ScheduleView,
    /// Where the arrow keys and Enter go.
    focus: Focus,
    stall: StallWatch,
    /// Display number of each channel (same order as the playlist).
    numbers: Vec<u32>,
    digits: DigitEntry,
    /// Channel played before `current` (`Backspace` goes back to it).
    previous: Option<usize>,
    /// Channel last started; `R` restarts it, also after a failure.
    current: Option<usize>,
    /// Channel logos; created once the window's context is known.
    logos: Option<Logos>,
    /// Some channel has a logo, so rows reserve room for one.
    show_logos: bool,
    /// The window can be kept on top by the app (not on Wayland).
    on_top_supported: bool,
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
    /// The window geometry was written to the debug log.
    window_logged: bool,
    /// Play the restored last channel once the startup load finishes.
    resume_on_load: bool,
    /// The cache folder: logos and saved copies of downloaded sources
    /// (`None`: nothing is cached on disk).
    cache_root: Option<PathBuf>,
    /// When the playlist and guide were downloaded (Unix seconds); `None`
    /// for local files and before the first load.
    playlist_downloaded_at: Option<i64>,
    epg_downloaded_at: Option<i64>,
    /// Last time a load started (Unix seconds), to space out update retries.
    load_started_at: i64,
}

impl App {
    /// `args`: optional playlist and EPG sources (they win over saved settings).
    /// With neither args nor saved settings, synthetic demo data is shown.
    pub fn from_args(args: &[String]) -> Self {
        let config_path = config::default_path();
        let host_config = config::host_config_path(|name| std::env::var(name).ok());
        let config = config_path
            .as_deref()
            .map(|path| config::load_or_import(path, host_config.as_deref()))
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
        app.on_top_supported = platform::can_keep_on_top(platform::is_wayland());
        app.autoload = !playlist_input.is_empty();
        app.cache_root = cache::default_root();
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
        let numbers = channel_numbers(&playlist);
        Self {
            numbers,
            digits: DigitEntry::default(),
            previous: None,
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
            archive: None,
            shown_at: 0,
            paused: None,
            reload_epg: false,
            audio_errors: ErrorBurst::default(),
            audio_disabled: false,
            http_error: None,
            failure: None,
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
            subtitles: Vec::new(),
            sub_pos_applied: None,
            tracks_refreshed_at: None,
            scroll_to: None,
            list_was_shown: false,
            view_pending: true,
            on_top_applied: None,
            // Real value set in `from_args`; tests stay independent of the session.
            on_top_supported: true,
            schedule: ScheduleView::default(),
            focus: Focus::default(),
            stall: StallWatch::default(),
            current: None,
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
            window_logged: false,
            resume_on_load: false,
            cache_root: None,
            playlist_downloaded_at: None,
            epg_downloaded_at: None,
            load_started_at: 0,
            video: VideoView::default(),
            ctx: egui::Context::default(),
            log: None,
            settings: None,
            file_pick: None,
        }
    }

    /// Loads the startup sources as their update policies say.
    fn load_by_policy(&mut self, ctx: &egui::Context) {
        let playlist = Fetch::Policy(self.config.playlist_update);
        let epg = Fetch::Policy(self.config.epg_update);
        self.start_load(ctx, playlist, epg);
    }

    /// Starts loading the current inputs on a background thread, replacing
    /// (and stopping) a load still in progress.
    fn start_load(&mut self, ctx: &egui::Context, playlist: Fetch, epg: Fetch) {
        let now = unix_now();
        let request = loader::Request {
            playlist: self.playlist_input.trim().to_string(),
            epg: self.epg_input.trim().to_string(),
            playlist_fetch: playlist,
            epg_fetch: epg,
            now,
            cache_dir: self.cache_root.as_ref().map(|root| root.join("sources")),
            // Used when the playlist is not reloaded or fails; without one
            // all guide channels are kept.
            channels: (!self.playlist.channels.is_empty())
                .then(|| loader::channel_filter(&self.playlist)),
            archive_days: self
                .config
                .archive
                .then(|| catchup::max_days(&self.playlist)),
        };
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (stop, ctx) = (cancel.clone(), ctx.clone());
        std::thread::spawn(move || {
            loader::run(&request, &stop, |event| {
                // The receiver is gone if the app was closed meanwhile; that's fine.
                let _ = tx.send(event);
                ctx.request_repaint(); // wake the UI to pick up the result
            });
        });
        self.notices.clear();
        self.load_started_at = now;
        self.loading = Some(PendingLoad {
            rx,
            cancel,
            playlist_src: self.playlist_input.trim().to_string(),
            epg_src: self.epg_input.trim().to_string(),
            playlist_due: playlist != Fetch::Keep,
            epg_due: epg != Fetch::Keep,
        });
    }

    fn poll_loading(&mut self) {
        while let Some(pending) = &mut self.loading {
            match pending.rx.try_recv() {
                Ok(LoadEvent::Playlist(result)) => {
                    pending.playlist_due = false;
                    let (p, e) = (pending.playlist_src.clone(), pending.epg_src.clone());
                    self.apply_playlist(result, &p, &e);
                }
                Ok(LoadEvent::Epg(result)) => {
                    pending.epg_due = false;
                    self.apply_epg(result);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    if pending.playlist_due || pending.epg_due {
                        self.notices.push("Loading stopped unexpectedly".into());
                    }
                    self.loading = None;
                }
            }
        }
    }

    /// What is still loading, for the list header.
    fn loading_text(&self) -> Option<&'static str> {
        let pending = self.loading.as_ref()?;
        Some(if pending.playlist_due {
            "Loading playlist…"
        } else {
            "Loading guide…"
        })
    }

    /// Starts updating what is due while the app runs (see `update_due`).
    /// After a failed try (the saved copy stays in use) it waits a while.
    fn check_updates(&mut self, ctx: &egui::Context, now: i64) {
        if self.loading.is_some() || now - self.load_started_at < UPDATE_RETRY_SECS {
            return;
        }
        let due = |policy, at| {
            if loader::update_due(policy, at, now) {
                Fetch::Download
            } else {
                Fetch::Keep
            }
        };
        let playlist = due(self.config.playlist_update, self.playlist_downloaded_at);
        let epg = due(self.config.epg_update, self.epg_downloaded_at);
        if playlist != Fetch::Keep || epg != Fetch::Keep {
            self.dlog(&format!("updating: playlist {playlist:?}, guide {epg:?}"));
            self.start_load(ctx, playlist, epg);
            self.load_started_at = now;
        }
    }

    fn apply_playlist(
        &mut self,
        result: Result<Fetched<Playlist>, String>,
        playlist_src: &str,
        epg_src: &str,
    ) {
        self.dlog(&match &result {
            Ok(f) => format!(
                "loaded playlist: {} channels{}",
                f.data.channels.len(),
                source_note(f)
            ),
            Err(e) => format!("playlist error: {e}"),
        });
        if let Ok(f) = &result {
            self.dlog(&catchup::summary(&f.data));
        }
        match result {
            Ok(fetched) => {
                if let Some(why) = &fetched.stale {
                    self.notices.push(stale_notice("Playlist", why, &fetched));
                }
                self.playlist_downloaded_at = fetched.downloaded_at;
                self.set_playlist(fetched.data);
                if std::mem::take(&mut self.resume_on_load) {
                    self.pending_play = self.selected;
                }
                self.config.playlist = Some(playlist_src.to_string());
                self.config.epg = (!epg_src.is_empty()).then(|| epg_src.to_string());
                self.pending_save = true;
            }
            Err(e) => self.notices.push(format!("Playlist: {e}")),
        }
        self.rebuild_guide();
    }

    fn apply_epg(&mut self, result: Option<Result<Fetched<Epg>, String>>) {
        self.dlog(&match &result {
            None => "guide: none".to_string(),
            Some(Ok(f)) => format!(
                "loaded guide: {} channels, {} programmes{}",
                f.data.channels.len(),
                f.data.programmes.len(),
                source_note(f)
            ),
            Some(Err(e)) => format!("guide error: {e}"),
        });
        match result {
            Some(Ok(fetched)) => {
                if let Some(why) = &fetched.stale {
                    self.notices.push(stale_notice("EPG", why, &fetched));
                }
                self.epg_downloaded_at = fetched.downloaded_at;
                self.epg = fetched.data;
            }
            Some(Err(e)) => self.notices.push(format!("EPG: {e}")),
            None => {
                self.epg = Epg::default();
                self.epg_downloaded_at = None;
            }
        }
        self.rebuild_guide();
    }

    fn rebuild_guide(&mut self) {
        self.guide = GuideIndex::build(&self.playlist, &self.epg);
        self.apply_shifts();
        self.show_logos = has_logos(&self.guide, self.playlist.channels.len());
    }

    /// Texture of the channel's logo, if it has one and it is loaded.
    fn logo_texture(&self, idx: usize) -> Option<egui::TextureHandle> {
        let url = self.guide.logo(idx)?;
        self.logos.as_ref()?.texture(&self.ctx, url)
    }

    /// Takes a new playlist. Channels referred to (selected, playing, …)
    /// are found again by key; an update may reorder or drop channels.
    fn set_playlist(&mut self, playlist: Playlist) {
        let keys = |idx: Option<usize>| -> Option<String> {
            Some(self.playlist.channels.get(idx?)?.key().to_string())
        };
        let (selected, playing) = (keys(self.selected), keys(self.playing));
        let (current, previous) = (keys(self.current), keys(self.previous));
        self.groups = groups(&playlist);
        self.numbers = channel_numbers(&playlist);
        self.playlist = playlist;
        let find = |key: Option<String>| -> Option<usize> {
            let key = key?;
            self.playlist.channels.iter().position(|c| c.key() == key)
        };
        // Else the channel played last time, if it is still there.
        self.selected = find(selected).or_else(|| find(self.config.last_channel.clone()));
        self.playing = find(playing);
        self.current = find(current);
        self.previous = find(previous);
        if self
            .group
            .as_ref()
            .is_some_and(|g| !self.groups.contains(g))
        {
            self.group = None;
        }
        self.refilter();
        self.scroll_to = self.selected;
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
        if draft.archive != self.config.archive {
            self.config.archive = draft.archive;
            self.reload_epg = true;
        }
        self.config.playlist_update = draft.playlist_update;
        self.config.epg_update = draft.epg_update;
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
        palette::install(&ctx);
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
        // Warnings and errors are always read: they explain failures and
        // reveal broken audio. The debug log gets everything.
        let level = if self.log.is_some() { "v" } else { "warn" };
        if let Err(e) = player.request_log_messages(level) {
            self.dlog(&format!("cannot get mpv log: {e}"));
        }
        self.dlog("player created, video attached");
        // Buffering state for the overlay; without it playback still works.
        for (name, kind) in [
            ("paused-for-cache", PropertyKind::Flag),
            ("cache-buffering-state", PropertyKind::Int),
        ] {
            if let Err(e) = player.observe(name, kind) {
                self.notices.push(format!("Cannot watch {name}: {e}"));
            }
        }
        self.player = Some(player);
        true
    }

    /// Releases the renderer and then mpv (GL context must be current).
    fn release_player(&mut self) {
        self.sub_pos_applied = None;
        self.video.detach();
        self.player = None;
        self.playing = None;
    }

    /// Plays a channel live.
    fn play(&mut self, idx: usize) {
        let Some(channel) = self.playlist.channels.get(idx) else {
            return;
        };
        self.dlog(&format!("play channel #{idx}: {}", channel.name));
        let url = channel.url.clone();
        if self.start_stream(idx, &url) {
            self.archive = None;
        }
    }

    /// Moves the guide of channels with a time shift (cheap: only their
    /// programmes are copied, the index is not rebuilt).
    fn apply_shifts(&mut self) {
        let shifts: Vec<i64> = (0..self.playlist.channels.len())
            .map(|i| self.shift_minutes(i) * 60)
            .collect();
        self.guide.apply_shifts(&mut self.epg, &shifts);
    }

    /// Guide time shift of the channel in minutes: the user's choice, else
    /// the playlist's `tvg-shift`.
    fn shift_minutes(&self, idx: usize) -> i64 {
        let Some(channel) = self.playlist.channels.get(idx) else {
            return 0;
        };
        self.config
            .channel_shift
            .get(channel.key())
            .copied()
            .unwrap_or_else(|| channel.tvg_shift.unwrap_or(0) / 60)
    }

    /// Remembers the guide time shift for a channel and moves its guide.
    fn set_shift(&mut self, idx: usize, minutes: i64) {
        let Some(channel) = self.playlist.channels.get(idx) else {
            return;
        };
        let key = channel.key().to_string();
        let minutes = minutes.clamp(-config::MAX_SHIFT_MIN, config::MAX_SHIFT_MIN);
        self.dlog(&format!("time shift of #{idx}: {minutes} min"));
        // The playlist's own value needs no entry.
        if minutes == channel.tvg_shift.unwrap_or(0) / 60 {
            self.config.channel_shift.remove(&key);
        } else {
            self.config.channel_shift.insert(key, minutes);
        }
        self.pending_save = true;
        self.apply_shifts();
    }

    /// The channel's archive, if it has one and the archive is turned on.
    fn archive_of(&self, idx: usize) -> Option<&Catchup> {
        let channel = self.playlist.channels.get(idx)?;
        channel.catchup.as_ref().filter(|_| self.config.archive)
    }

    /// Short archive mark for the channel ("⏪ 7d"), if it has an archive.
    fn archive_badge(&self, idx: usize) -> Option<String> {
        self.archive_of(idx).map(|a| format!("⏪ {}d", a.days))
    }

    /// Plays channel `idx` from the archive at the moment `at`.
    fn play_archive(&mut self, idx: usize, at: i64, now: i64, time: f64) {
        let list = self.guide.schedule(&self.epg, idx);
        let programme = timeshift::programme_at(&list, at);
        // Templates may want the programme's end; without a guide, now.
        let stop = programme.map_or(now, |p| p.stop);
        let title = programme.map(|p| p.title.clone());
        let (Some(channel), Some(archive)) =
            (self.playlist.channels.get(idx), self.archive_of(idx))
        else {
            return;
        };
        if !archive.covers(at, now) {
            self.show_osd("Not in the archive", time);
            return;
        }
        let Some(url) = archive.url(&channel.url, at, stop, now) else {
            self.show_osd("No archive link for this channel", time);
            return;
        };
        self.dlog(&format!(
            "play archive of #{idx}: {} at {at} ({:?})",
            channel.name, archive.mode
        ));
        if self.start_stream(idx, &url) {
            self.selected = Some(idx);
            self.archive = Some(Position::new(at));
            self.shown_at = at;
            let when = clock::local_hm(at);
            let text = match title {
                Some(t) => format!("Archive {when}: {t}"),
                None => format!("Archive {when}"),
            };
            self.show_osd(text, time);
        }
    }

    /// The moment on screen, read from the player for archive streams.
    fn update_position(&mut self, now: i64) {
        self.shown_at = match (self.paused, &mut self.archive) {
            (Some((_, at)), _) => at,
            (None, None) => now,
            (None, Some(pos)) => {
                let started = self.stream == StreamState::Playing && self.playing.is_some();
                let time = self
                    .player
                    .as_ref()
                    .filter(|_| started)
                    .and_then(|p| p.get_property("playback-time"))
                    .and_then(|t| t.parse::<f64>().ok());
                if started {
                    pos.at(time)
                } else {
                    // Opening or stopped: stay where it was.
                    self.shown_at.max(pos.origin)
                }
            }
        };
    }

    /// The channel playing (or last started) and its archive depth, for
    /// timeshift; says so on screen if there is no archive.
    fn timeshift_channel(&mut self, time: f64) -> Option<(usize, u32)> {
        let idx = self.playing.or(self.current)?;
        match self.archive_of(idx) {
            Some(a) => Some((idx, a.days)),
            None => {
                self.show_osd("This channel has no archive", time);
                None
            }
        }
    }

    /// Goes to a moment of the channel playing: live or the archive.
    fn go_to(&mut self, wanted: i64, now: i64, time: f64) {
        let Some((idx, days)) = self.timeshift_channel(time) else {
            return;
        };
        match timeshift::target(wanted, now, days) {
            Target::Live => {
                if self.archive.is_some() || self.playing.is_none() || self.paused.is_some() {
                    self.pending_play = Some(idx);
                }
                self.show_osd("Live", time);
            }
            Target::At(at) => self.play_archive(idx, at, now, time),
        }
    }

    /// Jumps to a programme boundary found by `find` from the moment shown.
    fn go_to_programme(&mut self, find: Find, now: i64, time: f64) {
        let Some(idx) = self.playing.or(self.current) else {
            return;
        };
        let list = self.guide.schedule(&self.epg, idx);
        let found = find(&list, self.shown_at);
        let empty = list.is_empty();
        match found {
            Some(at) => self.go_to(at, now, time),
            None if empty => self.show_osd("No programme guide for this channel", time),
            None => self.go_to(now, now, time),
        }
    }

    /// Pauses, or resumes: after a long pause on a channel with an archive,
    /// from the archive at the moment it was paused.
    fn toggle_pause(&mut self, now: i64, time: f64) {
        let Some(player) = self.player.as_ref().filter(|_| self.playing.is_some()) else {
            return;
        };
        match self.paused {
            None => {
                if let Err(e) = player.set_pause(true) {
                    self.status = format!("Cannot pause: {e}");
                    return;
                }
                self.paused = Some((time, self.shown_at));
                self.show_osd("Paused", time);
            }
            Some((since, at)) => {
                let has_archive = self.playing.and_then(|i| self.archive_of(i)).is_some();
                if has_archive && time - since > timeshift::LONG_PAUSE_SECS {
                    self.go_to(at, now, time);
                    return;
                }
                if let Err(e) = player.set_pause(false) {
                    self.status = format!("Cannot resume: {e}");
                    return;
                }
                self.paused = None;
                self.show_osd("Playing", time);
            }
        }
    }

    /// Opens `url` as channel `idx`; false if it could not be started.
    fn start_stream(&mut self, idx: usize, url: &str) -> bool {
        if !self.ensure_player() {
            return false;
        }
        let Some(channel) = self.playlist.channels.get(idx) else {
            return false;
        };
        let aspect = self.config.aspect_for(channel.key());
        if let Some(Err(e)) = self.player.as_ref().map(|p| p.set_aspect(aspect)) {
            self.dlog(&format!("cannot set picture size: {e}"));
        }
        let result = self.player.as_ref().map(|p| {
            p.load(url)?;
            // A pause would carry over to the new stream.
            p.set_pause(false)
        });
        self.paused = None;
        match result {
            Some(Ok(())) => {
                self.config.last_channel = Some(channel.key().to_string());
                self.pending_save = true;
                self.playing = Some(idx);
                if self.current != Some(idx) {
                    self.previous = self.current;
                }
                self.current = Some(idx);
                self.status = "Loading...".into();
                self.apply_volume();
                true
            }
            Some(Err(e)) => {
                self.status = format!("Cannot start playback: {e}");
                false
            }
            None => false,
        }
    }

    fn stop(&mut self) {
        if let Some(Err(e)) = self.player.as_ref().map(Player::stop) {
            self.status = format!("Cannot stop: {e}");
            return;
        }
        self.playing = None;
        self.archive = None;
        self.paused = None;
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
        self.dlog(&format!("mute: {}", self.muted));
        self.apply_volume();
        let text = if self.muted { "Muted" } else { "Sound on" };
        self.show_osd(text, now);
    }

    fn change_volume(&mut self, delta: f64, now: f64) {
        self.volume = (self.volume + delta).clamp(0.0, 100.0);
        self.dlog(&format!("volume: {:.0}%", self.volume));
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
                self.subtitles.clear();
                self.tracks_refreshed_at = None;
                self.stream = StreamState::Opening;
                self.status = "Loading...".into();
                self.audio_errors.clear();
                self.audio_disabled = false;
                self.http_error = None;
                self.failure = None;
            }
            PlayerEvent::Started => {
                self.info.stream_started();
                self.bar.request(BAR_START_SECS);
                self.tracks_refreshed_at = None;
                self.stream = StreamState::Playing;
                self.status = "Playing".into();
            }
            PlayerEvent::Property { name, value } => self.apply_property(&name, value),
            PlayerEvent::Log { prefix, text, .. } => self.apply_log(&prefix, &text),
            // Also fired when a new channel replaces the old one, so `playing`
            // is left alone; it is cleared by `stop` and by failures.
            // A stream given up on is stopped; keep showing why.
            PlayerEvent::Ended if matches!(self.stream, StreamState::Failed(_)) => {}
            PlayerEvent::Ended => {
                self.stream = StreamState::Idle;
                self.status = "Stopped".into();
                self.continue_archive();
            }
            PlayerEvent::Shutdown => {
                // The next Play creates a fresh instance.
                self.release_player();
                self.stream = StreamState::Idle;
                self.status = "Player closed. Press Play to start again.".into();
            }
            PlayerEvent::Failed(msg) => {
                self.info.reset();
                self.playing = None;
                self.failure = Some(msg);
                self.show_failure();
            }
        }
    }

    /// An archive stream that ends (some schemes stop at the programme's
    /// end) goes on from where it stopped, or live once it has caught up.
    fn continue_archive(&mut self) {
        let (Some(pos), Some(idx)) = (self.archive, self.playing) else {
            return;
        };
        // Ended without playing anything: not worth trying again.
        if self.shown_at < pos.origin + 10 {
            return;
        }
        self.actions.push(Action::SeekTo(self.shown_at));
        self.dlog(&format!("archive of #{idx} ended, going on"));
    }

    /// Shows the last failure with what is known about it.
    fn show_failure(&mut self) {
        let Some(msg) = &self.failure else {
            return;
        };
        let text = diagnose::failure_text(msg, self.http_error, self.archive.is_some());
        self.status = format!("Playback failed: {text}");
        self.stream = StreamState::Failed(text);
    }

    /// Looks for known problems in mpv's log.
    fn apply_log(&mut self, prefix: &str, text: &str) {
        if let Some(code) = diagnose::http_status(prefix, text) {
            self.http_error = Some(code);
            // The log line may come after the failure itself.
            if matches!(self.stream, StreamState::Failed(_)) {
                self.show_failure();
            }
        }
        if diagnose::is_audio_decode_error(prefix, text) && !self.audio_disabled {
            let time = self.ctx.input(|i| i.time);
            if self.audio_errors.record(time) {
                self.audio_disabled = true;
                self.dlog("audio cannot be decoded: playing without sound");
                if let Some(Err(e)) = self.player.as_ref().map(Player::disable_audio) {
                    self.dlog(&format!("cannot turn audio off: {e}"));
                }
                self.show_osd_for(
                    "The sound of this channel cannot be decoded: playing without it",
                    time,
                    AUDIO_OFF_OSD_SECS,
                );
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
        // An open menu (groups, tracks) takes the keys itself.
        if egui::Popup::is_any_open(ctx) {
            return;
        }
        // A clicked button keeps egui's keyboard focus, and Space or Enter
        // would press it again besides the shortcut: let go of it.
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        // Not reached while a text field has the keyboard, so typed
        // addresses never get into the log.
        if self.log.is_some()
            && let Some(line) = ctx.input(|i| pressed_keys_line(&i.events))
        {
            self.dlog(&line);
        }
        let pressed = |key| ctx.input(|i| i.key_pressed(key));
        let time = ctx.input(|i| i.time);
        if pressed(egui::Key::F) {
            self.set_fullscreen(ctx, !is_fullscreen(ctx));
        }
        const DIGITS: [(egui::Key, char); 10] = [
            (egui::Key::Num0, '0'),
            (egui::Key::Num1, '1'),
            (egui::Key::Num2, '2'),
            (egui::Key::Num3, '3'),
            (egui::Key::Num4, '4'),
            (egui::Key::Num5, '5'),
            (egui::Key::Num6, '6'),
            (egui::Key::Num7, '7'),
            (egui::Key::Num8, '8'),
            (egui::Key::Num9, '9'),
        ];
        for (key, digit) in DIGITS {
            if pressed(key) {
                self.digits.push(digit, time);
            }
        }
        if pressed(egui::Key::Backspace) && !self.digits.back(time) {
            self.go_back();
        }
        // Esc cancels typing, then closes the guide, then leaves fullscreen.
        // Esc cancels typing, then leaves the list or the guide, then fullscreen.
        if pressed(egui::Key::Escape)
            && !self.digits.cancel()
            && !self.leave_focus()
            && is_fullscreen(ctx)
        {
            self.set_fullscreen(ctx, false);
        }
        if pressed(egui::Key::G) {
            self.toggle_schedule();
        }
        if pressed(egui::Key::R) {
            self.restart();
        }
        if pressed(egui::Key::M) {
            self.toggle_mute(time);
        }
        if pressed(egui::Key::I) {
            self.info.cycle();
        }
        if pressed(egui::Key::L) {
            self.toggle_list(time);
        }
        if pressed(egui::Key::T) {
            self.toggle_on_top(time);
        }
        if pressed(egui::Key::Z) {
            self.set_aspect(self.current_aspect().next(), time);
        }
        if pressed(egui::Key::A) {
            self.next_audio_track(time);
        }
        if pressed(egui::Key::S) {
            self.next_subtitle(time);
        }
        if pressed(egui::Key::Enter) {
            match self.digits.finish(time, true) {
                Some(n) => self.jump_to_number(n, time),
                None => self.enter(unix_now()),
            }
        }
        let (up, down) = (pressed(egui::Key::ArrowUp), pressed(egui::Key::ArrowDown));
        if up || down {
            let delta = if up { -1 } else { 1 };
            match self.focus {
                Focus::Video => self.switch_channel(delta),
                Focus::List => self.move_selection(delta),
                Focus::Guide => self.schedule_cursor(delta, unix_now()),
            }
        }
        if pressed(egui::Key::Space) {
            self.actions.push(Action::TogglePause);
        }
        // Timeshift: 15 s, with Shift 1 min, with Ctrl 10 min.
        let step = ctx.input(|i| {
            if i.modifiers.command {
                600
            } else if i.modifiers.shift {
                60
            } else {
                15
            }
        });
        let (left, right) = (
            pressed(egui::Key::ArrowLeft),
            pressed(egui::Key::ArrowRight),
        );
        if self.focus == Focus::List {
            if left || right {
                self.cycle_group(if left { -1 } else { 1 }, time);
            }
        } else if left {
            self.actions.push(Action::Seek(-step));
        } else if right {
            self.actions.push(Action::Seek(step));
        }
        if pressed(egui::Key::Home) {
            self.actions.push(Action::ProgrammeStart);
        }
        if pressed(egui::Key::End) {
            self.actions.push(Action::BackToLive);
        }
        if pressed(egui::Key::PageUp) {
            self.switch_channel(-1);
        }
        if pressed(egui::Key::PageDown) {
            self.switch_channel(1);
        }
    }

    /// Writes the window geometry to the debug log (`--debug`).
    fn log_window(&self, when: &str, ctx: &egui::Context) {
        if self.log.is_none() {
            return;
        }
        let info = ctx.input(|i| i.viewport().clone());
        self.dlog(&format!(
            "window at {when}: inner {:?}, outer {:?}, monitor {:?}, scale {:?}, wayland {}",
            info.inner_rect,
            info.outer_rect,
            info.monitor_size,
            info.native_pixels_per_point,
            platform::is_wayland(),
        ));
    }

    /// Opens a file dialog the settings asked for, and puts a chosen file
    /// into its field once the dialog is done.
    fn update_file_pick(&mut self, ctx: &egui::Context) {
        let Some(draft) = &mut self.settings else {
            self.file_pick = None; // settings closed meanwhile: drop the result
            return;
        };
        if let Some(target) = draft.pick_request.take()
            && self.file_pick.is_none()
        {
            let current = match target {
                PickTarget::Playlist => &draft.playlist,
                PickTarget::Epg => &draft.epg,
            };
            self.file_pick = Some(filepick::open(target, current, ctx));
        }
        if let Some(pick) = &self.file_pick {
            match pick.poll() {
                PickState::Open => {}
                PickState::Chosen(path) => {
                    let path = path.to_string_lossy().into_owned();
                    match pick.target {
                        PickTarget::Playlist => draft.playlist = path,
                        PickTarget::Epg => draft.epg = path,
                    }
                    self.file_pick = None;
                }
                PickState::Cancelled => self.file_pick = None,
            }
        }
        if let Some(draft) = &mut self.settings {
            draft.picking = self.file_pick.is_some();
        }
    }

    /// Clears the cache when the settings dialog asks for it. Logos already
    /// on screen stay; saved sources are downloaded again next time.
    fn update_cache_request(&mut self) {
        let Some(draft) = &mut self.settings else {
            return;
        };
        if !std::mem::take(&mut draft.clear_cache) {
            return;
        }
        let Some(root) = &self.cache_root else {
            return;
        };
        let mut failed = None;
        for dir in ["logos", "sources"] {
            if let Err(e) = cache::clear(&root.join(dir)) {
                failed = Some(e);
            }
        }
        draft.cache_size = Some(cache::size(root));
        self.dlog(&format!("cache cleared: {failed:?}"));
        if let Some(e) = failed {
            self.notices
                .push(format!("Cannot clear the cache completely: {e}"));
        }
    }

    /// Gives up on a stream that has shown no picture for `STALL_SECS`.
    fn check_stall(&mut self, time: f64) {
        let waiting = match self.stream {
            StreamState::Opening | StreamState::Buffering(_) => true,
            // Open, but no frame yet: unless it is a radio channel.
            StreamState::Playing => !self.video.has_frame() && self.stream_has_video(),
            StreamState::Idle | StreamState::Failed(_) => false,
        };
        if !self.stall.timed_out(time, waiting) {
            return;
        }
        self.dlog("stream stalled, giving up");
        self.stream = StreamState::Failed(STALL_MESSAGE.into());
        self.status = format!("Playback failed: {STALL_MESSAGE}");
        self.playing = None;
        self.info.reset();
        if let Some(Err(e)) = self.player.as_ref().map(Player::stop) {
            self.dlog(&format!("cannot stop stalled stream: {e}"));
        }
    }

    /// The stream has (or may still get) a video track. mpv reports `vid`
    /// as "no" for audio-only streams.
    fn stream_has_video(&self) -> bool {
        self.player
            .as_ref()
            .and_then(|p| p.get_property("vid"))
            .is_none_or(|vid| vid != "no")
    }

    /// Takes a typed channel number once the user stops typing.
    fn update_digits(&mut self, ctx: &egui::Context) {
        let time = ctx.input(|i| i.time);
        if let Some(n) = self.digits.finish(time, false) {
            self.jump_to_number(n, time);
        }
        if let Some(secs) = self.digits.remaining(time) {
            ctx.request_repaint_after(Duration::from_secs_f64(secs.max(0.05)));
        }
    }

    fn jump_to_number(&mut self, n: u32, time: f64) {
        match find_number(&self.numbers, n) {
            Some(idx) => self.select_and_play(idx),
            None => self.show_osd(format!("No channel {n}"), time),
        }
    }

    /// Back to the channel played before the current one.
    fn go_back(&mut self) {
        if let Some(idx) = self.previous {
            self.select_and_play(idx);
        }
    }

    /// The channel the guide (bar and guide panel) is about: the one
    /// selected in the list, else the one playing.
    fn guide_channel(&self) -> Option<usize> {
        self.selected.or(self.playing)
    }

    /// Back to the channel playing: it becomes the selected one (so the
    /// guide is about it again) and the list scrolls to it.
    fn focus_current(&mut self) {
        if let Some(idx) = self.playing {
            self.selected = Some(idx);
        }
        self.scroll_to = self.selected;
    }

    /// Opening the guide panel starts at the channel playing.
    fn toggle_schedule(&mut self) {
        self.schedule.toggle();
        if self.schedule.open {
            self.focus_current();
            self.focus = Focus::Guide;
        } else if self.focus == Focus::Guide {
            self.focus = Focus::Video;
        }
    }

    /// Enter: opens the list from the video, plays from the list, opens a
    /// programme (or watches an open one) in the guide.
    fn enter(&mut self, now: i64) {
        match self.focus {
            Focus::Video => {
                self.focus_current();
                self.focus = Focus::List;
            }
            Focus::List => {
                if let Some(idx) = self.selected {
                    self.pending_play = Some(idx);
                    // Over the video, the list gets out of the way.
                    if self.list_floats() {
                        self.focus = Focus::Video;
                    }
                }
            }
            Focus::Guide => {
                let Some(idx) = self.guide_channel() else {
                    return;
                };
                let list = self.guide.schedule(&self.epg, idx);
                let archive = self.archive_of(idx).cloned();
                let in_archive =
                    |start: i64| archive.as_ref().is_some_and(|a| a.covers(start, now));
                if let Some(start) = self.schedule.activate(&list, now, in_archive) {
                    self.actions.push(Action::PlayArchive {
                        channel: idx,
                        start,
                    });
                }
            }
        }
    }

    /// Esc on the list or the guide: back to the video. False if the video
    /// had the keys already.
    fn leave_focus(&mut self) -> bool {
        match self.focus {
            Focus::Video => self.schedule.close(),
            Focus::List => {
                self.focus = Focus::Video;
                true
            }
            Focus::Guide => {
                self.schedule.close();
                self.focus = Focus::Video;
                true
            }
        }
    }

    /// The list is drawn over the video (fullscreen, or hidden in a window
    /// and called up with the keyboard).
    fn list_floats(&self) -> bool {
        self.fullscreen || !self.config.view.list_visible
    }

    /// ↑/↓ in the list: selects the channel above or below (no wrapping).
    fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let current = self.selected.or(self.playing);
        let pos = current.and_then(|c| self.visible.iter().position(|&i| i == c));
        let next = match pos {
            Some(p) => p.saturating_add_signed(delta).min(self.visible.len() - 1),
            None => 0,
        };
        let idx = self.visible[next];
        self.select(idx);
        self.scroll_to = Some(idx);
    }

    /// ←/→ in the list: the previous or next group ("All groups" first).
    fn cycle_group(&mut self, delta: isize, time: f64) {
        let count = self.groups.len() as isize + 1;
        let pos = match &self.group {
            None => 0,
            Some(g) => self
                .groups
                .iter()
                .position(|x| x == g)
                .map_or(0, |p| p as isize + 1),
        };
        let next = (pos + delta).rem_euclid(count);
        self.group = (next > 0).then(|| self.groups[next as usize - 1].clone());
        self.refilter();
        self.show_osd(
            self.group.as_deref().unwrap_or("All groups").to_string(),
            time,
        );
        // Keep the selection if the channel is in the group, else take the first.
        if !self.selected.is_some_and(|s| self.visible.contains(&s)) {
            self.selected = self.visible.first().copied();
        }
        self.scroll_to = self.selected;
    }

    /// ↑/↓ in the guide panel.
    fn schedule_cursor(&mut self, delta: isize, now: i64) {
        let Some(idx) = self.guide_channel() else {
            return;
        };
        let list = self.guide.schedule(&self.epg, idx);
        self.schedule.move_cursor(&list, now, delta);
    }

    /// Call once per frame with whether the channel list is on screen:
    /// when it comes up, it shows the channel playing.
    fn list_on_screen(&mut self, shown: bool) {
        let appeared = shown && !self.list_was_shown;
        self.list_was_shown = shown;
        if appeared {
            self.focus_current();
        }
    }

    /// Selects a channel in the list without playing it; the bar comes up
    /// with its programme.
    fn select(&mut self, idx: usize) {
        self.selected = Some(idx);
        self.bar.request(BAR_START_SECS);
    }

    fn select_and_play(&mut self, idx: usize) {
        self.selected = Some(idx);
        self.pending_play = Some(idx);
        self.scroll_to = Some(idx);
    }

    /// Plays the current (or last failed) channel again from scratch; an
    /// archived programme from its start.
    fn restart(&mut self) {
        let Some(idx) = self.playing.or(self.current) else {
            return;
        };
        match self.archive {
            Some(_) => self.actions.push(Action::PlayArchive {
                channel: idx,
                start: self.shown_at,
            }),
            None => self.pending_play = Some(idx),
        }
    }

    /// The moment shown from the archive, if channel `idx` is playing from it.
    fn archive_shown(&self, idx: usize) -> Option<i64> {
        self.archive
            .filter(|_| self.playing == Some(idx))
            .map(|_| self.shown_at)
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
                Action::ToggleSchedule => self.toggle_schedule(),
                Action::Restart => self.restart(),
                Action::ToggleFullscreen => self.set_fullscreen(ctx, !is_fullscreen(ctx)),
                Action::SetAspect(mode) => self.set_aspect(mode, time),
                Action::SetAudio(id) => self.select_audio_track(id, time),
                Action::SetSubtitle(id) => self.select_subtitle(id, time),
                Action::OpenSettings => {
                    let mut draft =
                        SettingsDraft::new(&self.playlist_input, &self.epg_input, &self.config);
                    draft.cache_size = self.cache_root.as_deref().map(cache::size);
                    self.settings = Some(draft);
                }
                Action::Reload => self.start_load(ctx, Fetch::Download, Fetch::Download),
                Action::PlayArchive { channel, start } => {
                    self.play_archive(channel, start, unix_now(), time);
                }
                Action::Seek(delta) => self.go_to(self.shown_at + delta, unix_now(), time),
                Action::SeekTo(at) => self.go_to(at, unix_now(), time),
                Action::ProgrammeStart => {
                    self.go_to_programme(timeshift::programme_start, unix_now(), time);
                }
                Action::NextProgramme => {
                    self.go_to_programme(timeshift::next_programme, unix_now(), time);
                }
                Action::TogglePause => self.toggle_pause(unix_now(), time),
                Action::SetShift { channel, minutes } => self.set_shift(channel, minutes),
                Action::BackToLive => {
                    if let Some(idx) = self.playing.or(self.current) {
                        self.pending_play = Some(idx);
                    }
                }
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
        self.show_osd_for(text, time, OSD_SECS);
    }

    fn show_osd_for(&mut self, text: impl Into<String>, time: f64, secs: f64) {
        self.osd = Some((text.into(), time + secs));
    }

    /// Windowed: docks or hides the list. Fullscreen: pins or unpins the
    /// list over the video.
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
        if !self.on_top_supported {
            self.show_osd(ON_TOP_UNAVAILABLE, time);
            return;
        }
        self.config.view.always_on_top = !self.config.view.always_on_top;
        self.pending_save = true;
        let text = if self.config.view.always_on_top {
            "Always on top"
        } else {
            "Normal window"
        };
        self.show_osd(text, time);
    }

    /// The channel on screen (or last started), whose picture size the
    /// controls change.
    fn screen_channel_key(&self) -> Option<String> {
        let idx = self.playing.or(self.current)?;
        Some(self.playlist.channels.get(idx)?.key().to_string())
    }

    /// Picture size of the channel on screen.
    fn current_aspect(&self) -> AspectMode {
        self.screen_channel_key()
            .map_or(AspectMode::Auto, |key| self.config.aspect_for(&key))
    }

    /// Changes the picture size, remembered for the channel on screen.
    fn set_aspect(&mut self, mode: AspectMode, time: f64) {
        if let Some(key) = self.screen_channel_key() {
            self.config.set_aspect_for(&key, mode);
            self.pending_save = true;
        }
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
            self.subtitles = tracks::subtitle_tracks(|name| player.get_property(name));
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

    /// Keeps subtitles above the bar: `covered` is the share (0–1) of the
    /// video height the bar takes right now.
    fn lift_subtitles(&mut self, covered: f32) {
        let pos = subtitle_position(covered);
        if self.subtitles.is_empty() || self.sub_pos_applied == Some(pos) {
            return;
        }
        if let Some(player) = &self.player
            && player.set_subtitle_position(pos).is_ok()
        {
            self.sub_pos_applied = Some(pos);
        }
    }

    fn next_subtitle(&mut self, time: f64) {
        self.tracks_refreshed_at = None;
        self.refresh_tracks(time);
        if self.subtitles.is_empty() {
            self.show_osd("No subtitles", time);
            return;
        }
        self.select_subtitle(tracks::next_subtitle(&self.subtitles), time);
    }

    /// Shows subtitle track `id`, or none.
    fn select_subtitle(&mut self, id: Option<i64>, time: f64) {
        let mut refused = false;
        if let Some(player) = &self.player {
            if let Err(e) = player.set_subtitle_track(id) {
                self.status = format!("Cannot switch subtitles: {e}");
                return;
            }
            refused = tracks::subtitle_refused(id, player.get_property("sid").as_deref());
        }
        let label = match id {
            None => "off".to_string(),
            Some(id) => self
                .subtitles
                .iter()
                .find(|t| t.id == id)
                .map_or_else(|| format!("Track {id}"), |t| t.label()),
        };
        for t in &mut self.subtitles {
            t.selected = !refused && Some(t.id) == id;
        }
        if refused {
            self.dlog(&format!("subtitles refused by mpv: {label}"));
            self.show_osd(format!("Cannot show subtitles: {label}"), time);
        } else {
            self.show_osd(format!("Subtitles: {label}"), time);
        }
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
        if self.on_top_supported {
            let on_top = wants_on_top(self.config.view.always_on_top, is_fullscreen(ctx));
            self.send_on_top(ctx, on_top);
        }
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
        let ctx = self.ctx.clone();
        self.log_window("exit", &ctx);
        self.release_player();
    }

    /// eframe stores only the window size and position (`persistence`
    /// feature); widget state is not worth keeping between runs.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = unix_now();
        if !self.window_logged {
            self.window_logged = true;
            self.log_window("start", ui.ctx());
        }
        if std::mem::take(&mut self.autoload) {
            self.load_by_policy(ui.ctx());
        }
        self.poll_loading();
        self.check_updates(ui.ctx(), now);
        self.poll_player();
        self.update_position(now);
        self.check_stall(ui.input(|i| i.time));
        self.handle_shortcuts(ui.ctx());
        self.update_digits(ui.ctx());
        self.apply_view(ui.ctx());
        let fullscreen = is_fullscreen(ui.ctx());
        self.fullscreen = fullscreen;
        let mode = list_mode(
            fullscreen,
            self.config.view.list_visible,
            self.focus == Focus::List,
        );
        let screen = ui.max_rect();
        if mode == ListMode::Hidden {
            self.list_on_screen(false);
        }
        if mode == ListMode::Docked {
            self.list_on_screen(true);
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
        if mode == ListMode::Overlay {
            self.list_overlay(ui.ctx(), screen, now);
        } else {
            self.list_overlay.hide();
        }
        self.schedule_panel(ui.ctx(), screen, now);
        let ctx = ui.ctx().clone();
        self.update_file_pick(&ctx);
        self.update_cache_request();
        if let Some(draft) = &mut self.settings
            && let Some(outcome) = settings::show(&ctx, draft)
        {
            let draft = self.settings.take();
            if let (Outcome::Save, Some(draft)) = (outcome, draft) {
                if self.apply_settings(draft) {
                    self.load_by_policy(&ctx);
                } else if std::mem::take(&mut self.reload_epg) && !self.epg_input.is_empty() {
                    self.start_load(&ctx, Fetch::Keep, Fetch::Policy(self.config.epg_update));
                }
                self.reload_epg = false;
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

/// Where loaded data came from, for the debug log.
fn source_note<T>(fetched: &Fetched<T>) -> String {
    match (fetched.downloaded_at, fetched.from_saved) {
        (None, _) => " (file)".to_string(),
        (Some(at), true) => format!(" (saved copy from {})", when(at)),
        (Some(_), false) => " (downloaded)".to_string(),
    }
}

/// "EPG: network error; showing the copy saved Today 08:15".
fn stale_notice<T>(what: &str, why: &str, fetched: &Fetched<T>) -> String {
    let saved = fetched
        .downloaded_at
        .map_or_else(String::new, |at| format!(" saved {}", when(at)));
    format!("{what}: {why}; showing the copy{saved}")
}

/// "Today 08:15", "Thu 5 Nov 22:40".
fn when(unix: i64) -> String {
    let day = clock::day_label(clock::local_day(unix), clock::local_day(unix_now()));
    format!("{day} {}", clock::local_hm(unix))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Why nothing can be heard right now, for the reminder over the video
/// (`None`: sound is on).
fn silence_note(muted: bool, volume: f64, audio_disabled: bool) -> Option<&'static str> {
    if muted {
        Some("🔇 Muted (M)")
    } else if volume <= 0.0 {
        Some("🔇 Volume 0%")
    } else if audio_disabled {
        Some("🔇 No sound on this channel")
    } else {
        None
    }
}

/// The keys pressed in this frame, for the debug log (`None`: no keys).
/// Shows what a gamepad mapping such as Steam Input really sends.
fn pressed_keys_line(events: &[egui::Event]) -> Option<String> {
    let keys: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } if modifiers.is_none() => Some(key.name().to_string()),
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => Some(format!("{modifiers:?}+{}", key.name())),
            _ => None,
        })
        .collect();
    (!keys.is_empty()).then(|| format!("keys: {}", keys.join(" ")))
}

fn is_fullscreen(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
}

/// mpv `sub-pos` (percent from the top) that keeps subtitles above a bar
/// covering `covered` (0–1) of the video; never above the middle.
fn subtitle_position(covered: f32) -> u32 {
    (100.0 - covered.clamp(0.0, 0.5) * 100.0).round() as u32
}

fn has_logos(guide: &GuideIndex, channels: usize) -> bool {
    (0..channels).any(|i| guide.logo(i).is_some())
}

/// Starts moving the window when a drag with the left button begins on
/// `response` (the OS takes over the drag). Not in fullscreen.
fn drag_window(response: &egui::Response, fullscreen: bool) {
    if !fullscreen && response.drag_started_by(egui::PointerButton::Primary) {
        response
            .ctx
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}

/// Makes the empty background of `ui` a handle for moving the window.
/// Call it before adding widgets: those added later take the pointer first.
/// Labels stop being selectable, so dragging over text moves the window too.
fn window_drag_background(ui: &mut egui::Ui, fullscreen: bool) {
    ui.style_mut().interaction.selectable_labels = false;
    if fullscreen {
        return;
    }
    // Also senses clicks: a drag-only widget starts dragging on the press
    // itself, and once the OS has taken over the pointer, clicks (and
    // double-clicks) on the rows above it are lost.
    let response = ui.interact(
        ui.max_rect(),
        ui.unique_id().with("window-drag"),
        egui::Sense::click_and_drag(),
    );
    drag_window(&response, fullscreen);
}

/// Where the channel list goes: docked beside the video, sliding in over
/// it (fullscreen), or nowhere (windowed with the list hidden).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListMode {
    Docked,
    Overlay,
    Hidden,
}

/// `list_focused`: the keyboard is on the list, so a hidden list comes up
/// over the video.
fn list_mode(fullscreen: bool, list_visible: bool, list_focused: bool) -> ListMode {
    match (fullscreen, list_visible) {
        (true, _) => ListMode::Overlay,
        (false, true) => ListMode::Docked,
        (false, false) if list_focused => ListMode::Overlay,
        (false, false) => ListMode::Hidden,
    }
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
    // The news channel has a day of archive, to show how it looks.
    let archive = catchup::Attrs {
        mode: Some("shift".into()),
        days: Some("1".into()),
        source: None,
    };
    for (name, id, group) in entries {
        let url = format!("http://example.com/{id}.m3u8?token=fakekey");
        playlist.channels.push(Channel {
            name: name.into(),
            catchup: (id == "news.example")
                .then(|| archive.resolve(&url))
                .flatten(),
            url,
            tvg_id: Some(id.into()),
            group_title: Some(group.into()),
            ..Channel::default()
        });
        epg.channels.push(EpgChannel {
            id: id.into(),
            display_names: vec![name.into()],
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
    use openzap::config::UpdatePolicy;

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

    fn log(prefix: &str, text: &str) -> PlayerEvent {
        PlayerEvent::Log {
            prefix: prefix.into(),
            level: "error".into(),
            text: text.into(),
        }
    }

    #[test]
    fn archive_failure_names_the_archive_and_the_http_status() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.playing = Some(0);
        app.archive = Some(Position::new(-1800));
        app.apply_event(PlayerEvent::Loading);
        app.apply_event(PlayerEvent::Failed("loading failed".into()));
        assert!(
            app.status.contains("archive did not answer"),
            "{}",
            app.status
        );
        // mpv logs the HTTP error after reporting the failure.
        app.apply_event(log("curl", "HTTP error 502"));
        let StreamState::Failed(text) = &app.stream else {
            panic!("not failed: {:?}", app.stream);
        };
        assert!(
            text.contains("HTTP 502") && text.contains("archive"),
            "{text}"
        );

        // A new stream forgets it.
        app.apply_event(PlayerEvent::Loading);
        assert_eq!(app.http_error, None);
        app.archive = None;
        app.apply_event(PlayerEvent::Failed("loading failed".into()));
        assert_eq!(app.status, "Playback failed: loading failed");
    }

    #[test]
    fn broken_audio_is_turned_off_once() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.apply_event(PlayerEvent::Loading);
        for _ in 0..diagnose::AUDIO_ERROR_LIMIT - 1 {
            app.apply_event(log("ad", "Error decoding audio."));
        }
        assert!(!app.audio_disabled, "a few errors are tolerated");
        app.apply_event(log("ad", "Error decoding audio."));
        assert!(app.audio_disabled);
        assert!(app.osd.as_ref().is_some_and(|(t, _)| t.contains("sound")));
        app.osd = None;
        app.apply_event(log("ad", "Error decoding audio."));
        assert!(app.osd.is_none(), "reported once");
        app.apply_event(PlayerEvent::Loading);
        assert!(!app.audio_disabled, "the next stream has sound again");
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

    /// Both results of a load, as the loader sends them one by one.
    struct Loaded {
        playlist: Result<Playlist, String>,
        epg: Option<Result<Epg, String>>,
    }

    fn loaded(playlist: Result<Playlist, String>, epg: Option<Result<Epg, String>>) -> Loaded {
        Loaded { playlist, epg }
    }

    /// Data read from a local file.
    fn local<T>(data: T) -> Fetched<T> {
        Fetched {
            data,
            downloaded_at: None,
            from_saved: false,
            stale: None,
        }
    }

    impl App {
        fn apply_loaded(&mut self, loaded: Loaded, playlist_src: &str, epg_src: &str) {
            self.apply_playlist(loaded.playlist.map(local), playlist_src, epg_src);
            self.apply_epg(loaded.epg.map(|r| r.map(local)));
        }
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

        app.start_load(&ctx, Fetch::Download, Fetch::Download);
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
    fn a_new_load_stops_the_one_in_progress() {
        let ctx = egui::Context::default();
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.playlist_input = "/nonexistent/a.m3u".into();
        app.start_load(&ctx, Fetch::Download, Fetch::Download);
        let first = app.loading.as_ref().unwrap().cancel.clone();
        app.playlist_input = "/nonexistent/b.m3u".into();
        app.start_load(&ctx, Fetch::Download, Fetch::Keep);
        assert!(first.load(Ordering::Relaxed), "old load cancelled");
        let pending = app.loading.as_ref().unwrap();
        assert_eq!(pending.playlist_src, "/nonexistent/b.m3u");
        assert!(pending.playlist_due && !pending.epg_due);
    }

    #[test]
    fn the_playlist_shows_before_the_guide_arrives() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        let (tx, rx) = mpsc::channel();
        app.loading = Some(PendingLoad {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            playlist_src: "p".into(),
            epg_src: "e".into(),
            playlist_due: true,
            epg_due: true,
        });
        assert_eq!(app.loading_text(), Some("Loading playlist…"));
        tx.send(LoadEvent::Playlist(Ok(local(playlist)))).unwrap();
        app.poll_loading();
        assert_eq!(app.playlist.channels.len(), 3);
        assert_eq!(app.loading_text(), Some("Loading guide…"));
        tx.send(LoadEvent::Epg(Some(Ok(local(epg))))).unwrap();
        drop(tx);
        app.poll_loading();
        assert!(app.loading.is_none() && app.notices.is_empty());
        assert!(!app.epg.programmes.is_empty());
    }

    #[test]
    fn a_saved_copy_in_use_is_reported() {
        let (playlist, _) = demo_data(0);
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        let fetched = Fetched {
            data: playlist,
            downloaded_at: Some(1_000),
            from_saved: true,
            stale: Some("network error".into()),
        };
        app.apply_playlist(Ok(fetched), "http://example.com/l.m3u", "");
        assert_eq!(app.playlist.channels.len(), 3);
        assert_eq!(app.playlist_downloaded_at, Some(1_000));
        assert!(
            source_note(&Fetched {
                data: (),
                downloaded_at: Some(unix_now()),
                from_saved: true,
                stale: None,
            })
            .starts_with(" (saved copy from Today")
        );
        assert!(
            app.notices[0].starts_with("Playlist: network error; showing the copy saved "),
            "{:?}",
            app.notices
        );
    }

    #[test]
    fn due_updates_start_while_running_but_not_too_often() {
        let ctx = egui::Context::default();
        let day = loader::REFRESH_SECS;
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.playlist_input = "/nonexistent/a.m3u".into();
        app.epg_downloaded_at = Some(0);
        app.check_updates(&ctx, day - 1);
        assert!(app.loading.is_none(), "not a day old yet");
        app.check_updates(&ctx, day);
        let pending = app.loading.take().expect("guide update started");
        assert!(pending.epg_due && !pending.playlist_due);
        // It failed (the old copy stays): no new try right away.
        app.check_updates(&ctx, day + 10);
        assert!(app.loading.is_none());
        app.check_updates(&ctx, day + UPDATE_RETRY_SECS);
        assert!(app.loading.is_some());

        app.loading = None;
        app.config.epg_update = UpdatePolicy::Never;
        app.check_updates(&ctx, 10 * day);
        assert!(app.loading.is_none());
    }

    #[test]
    fn an_updated_playlist_keeps_selection_and_playing_channel() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist.clone(), epg, vec![]);
        app.selected = Some(2);
        app.playing = Some(1);
        app.current = Some(1);
        app.group = Some("Sports".into());
        // The update drops "Example News" and so moves everything up.
        let mut updated = playlist;
        updated.channels.remove(0);
        app.set_playlist(updated);
        assert_eq!(
            (app.selected, app.playing, app.current),
            (Some(1), Some(0), Some(0))
        );
        assert_eq!(app.group.as_deref(), Some("Sports"), "group still exists");
    }

    #[test]
    fn archive_is_offered_only_when_turned_on() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        assert!(app.archive_of(0).is_some(), "demo news has an archive");
        assert!(app.archive_of(1).is_none());
        assert_eq!(app.archive_badge(0).as_deref(), Some("⏪ 1d"));
        assert_eq!(app.archive_badge(1), None);
        app.config.archive = false;
        assert!(app.archive_of(0).is_none());
        assert_eq!(app.archive_badge(0), None);
    }

    #[test]
    fn restart_replays_the_archived_programme() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.playing = Some(0);
        app.restart();
        assert_eq!(app.pending_play, Some(0), "live");

        app.pending_play = None;
        app.archive = Some(Position::new(-1800));
        app.shown_at = -1200; // played on for ten minutes
        app.restart();
        assert_eq!(app.pending_play, None);
        assert_eq!(
            app.actions,
            [Action::PlayArchive {
                channel: 0,
                start: -1200
            }],
            "from where it is"
        );
        assert_eq!(app.archive_shown(0), Some(-1200));
        assert_eq!(app.archive_shown(1), None, "another channel is shown");

        app.stop();
        assert_eq!(app.archive, None);
    }

    #[test]
    fn timeshift_needs_an_archive_and_near_now_means_live() {
        let now = 100_000;
        let (playlist, epg) = demo_data(now);
        let mut app = App::new(playlist, epg, vec![]);
        // Channel 1 has no archive.
        app.playing = Some(1);
        app.go_to(now - 600, now, 0.0);
        assert!(
            app.osd
                .as_ref()
                .is_some_and(|(t, _)| t.contains("no archive"))
        );
        assert_eq!(app.pending_play, None);

        // Channel 0 does; forward past now while in the archive: live.
        app.playing = Some(0);
        app.archive = Some(Position::new(now - 600));
        app.go_to(now + 15, now, 0.0);
        assert_eq!(app.pending_play, Some(0));
    }

    #[test]
    fn position_stays_put_while_paused_or_opening() {
        let now = 100_000;
        let (playlist, epg) = demo_data(now);
        let mut app = App::new(playlist, epg, vec![]);
        app.update_position(now);
        assert_eq!(app.shown_at, now, "live");
        app.playing = Some(0);
        app.archive = Some(Position::new(now - 600));
        app.stream = StreamState::Opening;
        app.update_position(now);
        assert_eq!(app.shown_at, now, "opening: stays where it was");
        app.paused = Some((1.0, now - 300));
        app.update_position(now + 50);
        assert_eq!(app.shown_at, now - 300);
    }

    #[test]
    fn an_ended_archive_stream_goes_on() {
        let now = 100_000;
        let (playlist, epg) = demo_data(now);
        let mut app = App::new(playlist, epg, vec![]);
        app.playing = Some(0);
        app.archive = Some(Position::new(now - 3600));
        app.shown_at = now - 3600 + 5;
        app.apply_event(PlayerEvent::Ended);
        assert!(app.actions.is_empty(), "nothing played: no retry loop");
        app.shown_at = now - 1800;
        app.apply_event(PlayerEvent::Ended);
        assert_eq!(app.actions, [Action::SeekTo(now - 1800)]);
    }

    #[test]
    fn turning_the_archive_off_reloads_only_the_guide() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        let mut draft = SettingsDraft::new("", "", &app.config);
        assert!(!app.apply_settings(draft.clone()));
        assert!(!app.reload_epg, "nothing changed");
        draft.archive = false;
        assert!(!app.apply_settings(draft), "sources unchanged");
        assert!(app.reload_epg);
        assert!(!app.config.archive);
    }

    #[test]
    fn time_shift_moves_the_guide_and_is_remembered() {
        let now = 100_000;
        let (playlist, epg) = demo_data(now);
        let mut app = App::new(playlist, epg, vec![]);
        let first = |app: &App| app.guide.schedule(&app.epg, 0)[0].start;
        let before = first(&app);
        app.set_shift(0, 60);
        assert_eq!(first(&app), before + 3600);
        assert_eq!(app.config.channel_shift.get("news.example"), Some(&60));
        assert!(app.pending_save);
        app.set_shift(0, 0);
        assert_eq!(first(&app), before);
        assert!(
            app.config.channel_shift.is_empty(),
            "no shift is not stored"
        );
    }

    #[test]
    fn enter_and_escape_move_the_keyboard_between_video_and_list() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.playing = Some(1);
        app.enter(0);
        assert_eq!(app.focus, Focus::List);
        assert_eq!(app.selected, Some(1), "starts at the channel playing");
        app.move_selection(1);
        app.enter(0);
        assert_eq!(app.pending_play, Some(2));
        assert_eq!(app.focus, Focus::List, "a docked list keeps the keys");
        app.fullscreen = true;
        app.enter(0);
        assert_eq!(
            app.focus,
            Focus::Video,
            "over the video it gets out of the way"
        );
        app.focus = Focus::List;
        assert!(app.leave_focus());
        assert_eq!(app.focus, Focus::Video);
        assert!(!app.leave_focus(), "Esc goes on to leave fullscreen");
    }

    #[test]
    fn arrows_in_the_list_select_and_change_groups() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.focus = Focus::List;
        app.move_selection(-1);
        assert_eq!(app.selected, Some(0), "nothing selected: the first");
        app.move_selection(-1);
        assert_eq!(app.selected, Some(0), "no wrapping");
        app.move_selection(5);
        assert_eq!(app.selected, Some(2));
        // Groups: All → Movies → News → Sports → All.
        assert_eq!(app.groups, ["Movies", "News", "Sports"]);
        app.cycle_group(1, 0.0);
        assert_eq!(app.group.as_deref(), Some("Movies"));
        assert_eq!(
            app.selected,
            Some(1),
            "the selection follows into the group"
        );
        app.cycle_group(-1, 0.0);
        app.cycle_group(-1, 0.0);
        assert_eq!(app.group.as_deref(), Some("Sports"));
        assert_eq!(app.visible, [2]);
    }

    #[test]
    fn guide_keys_open_a_programme_and_then_watch_it() {
        let now = 100_000;
        let (playlist, epg) = demo_data(now);
        let mut app = App::new(playlist, epg, vec![]);
        app.selected = Some(0); // the news channel has an archive
        app.toggle_schedule();
        assert_eq!(app.focus, Focus::Guide);
        app.schedule_cursor(1, now);
        app.schedule_cursor(-1, now);
        // On the programme on air: Enter opens it, Enter again watches it.
        app.enter(now);
        assert!(app.actions.is_empty());
        app.enter(now);
        assert_eq!(
            app.actions,
            [Action::PlayArchive {
                channel: 0,
                start: now - 1800
            }]
        );
        assert!(app.leave_focus());
        assert!(!app.schedule.open);
        assert_eq!(app.focus, Focus::Video);
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
    fn stream_info_shows_briefly_on_start() {
        let mut info = InfoOverlay::default();
        assert_eq!(info.shown(0.0), InfoMode::Off);
        info.stream_started();
        assert_eq!(info.shown(10.0), InfoMode::Stream);
        assert_eq!(info.shown(10.0 + INFO_BRIEF_SECS - 0.1), InfoMode::Stream);
        assert_eq!(info.shown(10.0 + INFO_BRIEF_SECS), InfoMode::Off);
    }

    #[test]
    fn info_key_cycles_programme_stream_off() {
        let mut info = InfoOverlay::default();
        info.cycle();
        assert_eq!(info.shown(100.0), InfoMode::Programme);
        // The chosen panel stays over channel changes; no brief stream info.
        info.reset();
        info.stream_started();
        assert_eq!(info.shown(101.0), InfoMode::Programme);
        info.cycle();
        assert_eq!(info.shown(102.0), InfoMode::Stream);
        info.cycle();
        assert_eq!(info.shown(103.0), InfoMode::Off);
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
        assert_eq!(app.info.shown(1.0), InfoMode::Stream);
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
        assert!(app.pending_save && app.actions.is_empty());
        assert_eq!(
            app.osd.map(|(t, _)| t).as_deref(),
            Some("Picture: Fill (crop)")
        );
    }

    #[test]
    fn picture_size_is_remembered_per_channel() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.set_aspect(AspectMode::Fill, 0.0);
        assert!(app.config.channel_aspect.is_empty(), "nothing on screen");

        app.current = Some(1);
        app.set_aspect(AspectMode::Ratio4x3, 0.0);
        assert_eq!(app.current_aspect(), AspectMode::Ratio4x3);
        assert_eq!(app.config.aspect_for("film.example"), AspectMode::Ratio4x3);
        assert!(app.pending_save);
        app.current = Some(2);
        assert_eq!(
            app.current_aspect(),
            AspectMode::Auto,
            "other channels unchanged"
        );
        app.current = Some(1);
        app.set_aspect(AspectMode::Auto, 0.0);
        assert!(app.config.channel_aspect.is_empty());
    }

    #[test]
    fn on_top_is_not_faked_where_unsupported() {
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.on_top_supported = false;
        app.config.view.always_on_top = true; // e.g. saved on X11 earlier
        app.toggle_on_top(1.0);
        assert!(app.config.view.always_on_top, "setting left alone");
        assert_eq!(
            app.osd.as_ref().map(|(t, _)| t.as_str()),
            Some(ON_TOP_UNAVAILABLE)
        );
        app.apply_view(&egui::Context::default());
        assert_eq!(app.on_top_applied, None, "nothing sent to the window");
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
    fn list_is_docked_slides_in_or_stays_away() {
        assert_eq!(list_mode(false, true, false), ListMode::Docked);
        // Hidden in a window: no sliding in at the edge.
        assert_eq!(list_mode(false, false, false), ListMode::Hidden);
        assert_eq!(
            list_mode(false, false, true),
            ListMode::Overlay,
            "called up by keyboard"
        );
        assert_eq!(list_mode(true, true, false), ListMode::Overlay);
        assert_eq!(list_mode(true, false, false), ListMode::Overlay);
    }

    #[test]
    fn reopened_guide_returns_to_the_channel_playing() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.playing = Some(0);
        app.toggle_schedule();
        app.select(2); // browsing in the list moves the guide along
        assert_eq!(app.guide_channel(), Some(2));
        app.toggle_schedule(); // closed: nothing changes
        assert_eq!(app.guide_channel(), Some(2));
        app.toggle_schedule();
        assert_eq!(app.guide_channel(), Some(0));
        assert_eq!(app.scroll_to, Some(0));
    }

    #[test]
    fn list_coming_up_shows_the_channel_playing() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.playing = Some(1);
        app.list_on_screen(true);
        assert_eq!((app.selected, app.scroll_to), (Some(1), Some(1)));
        // While it stays up, the user's scrolling and selection are kept.
        app.scroll_to = None;
        app.select(2);
        app.list_on_screen(true);
        assert_eq!((app.selected, app.scroll_to), (Some(2), None));
        app.list_on_screen(false);
        app.list_on_screen(true);
        assert_eq!((app.selected, app.scroll_to), (Some(1), Some(1)));

        // Nothing playing: the selection stays and is scrolled to.
        app.playing = None;
        app.selected = Some(2);
        app.list_on_screen(false);
        app.list_on_screen(true);
        assert_eq!((app.selected, app.scroll_to), (Some(2), Some(2)));
    }

    #[test]
    fn guide_follows_the_selected_channel() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        assert_eq!(app.guide_channel(), None);
        app.playing = Some(0);
        assert_eq!(
            app.guide_channel(),
            Some(0),
            "nothing selected: the one playing"
        );
        app.select(2);
        assert_eq!(app.guide_channel(), Some(2));
        assert_eq!(app.pending_play, None, "a click only selects");
        assert!(
            app.bar.is_visible(10.0, false),
            "bar comes up with the guide"
        );
    }

    /// Presses the left button on a draggable rect and moves the pointer,
    /// one event per frame; true if a window drag was asked for.
    fn drag_requested(fullscreen: bool) -> bool {
        let ctx = egui::Context::default();
        let at = |x| egui::pos2(x, 50.0);
        let press = |pressed| egui::Event::PointerButton {
            pos: at(50.0),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let frames = [
            vec![egui::Event::PointerMoved(at(50.0))],
            vec![press(true)],
            vec![egui::Event::PointerMoved(at(90.0))],
            vec![],
        ];
        let mut asked = false;
        for events in frames {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 100.0),
                )),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                let response = ui.allocate_rect(ui.max_rect(), egui::Sense::click_and_drag());
                drag_window(&response, fullscreen);
            });
            output.textures_delta.clear(); // nothing renders them here
            asked |= output.viewport_output.values().any(|v| {
                v.commands
                    .iter()
                    .any(|c| matches!(c, egui::ViewportCommand::StartDrag))
            });
        }
        asked
    }

    /// A clickable row over a draggable background (as in the channel
    /// list); `events` go one per frame. Returns (double-clicked, window
    /// drag asked for).
    fn row_over_background(frames: Vec<Vec<egui::Event>>) -> (bool, bool) {
        let ctx = egui::Context::default();
        let (mut double, mut drag) = (false, false);
        for (i, events) in frames.into_iter().enumerate() {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 100.0),
                )),
                // Clicks a few frames apart, well within a double-click.
                time: Some(i as f64 * 0.05),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                window_drag_background(ui, false);
                let row = egui::Rect::from_min_size(egui::pos2(0.0, 20.0), egui::vec2(200.0, 40.0));
                double |= ui.allocate_rect(row, egui::Sense::click()).double_clicked();
            });
            output.textures_delta.clear();
            drag |= output.viewport_output.values().any(|v| {
                v.commands
                    .iter()
                    .any(|c| matches!(c, egui::ViewportCommand::StartDrag))
            });
        }
        (double, drag)
    }

    fn button(x: f32, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::pos2(x, 40.0),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn double_click_on_a_row_does_not_move_the_window() {
        let moved = egui::Event::PointerMoved(egui::pos2(50.0, 40.0));
        let (double, drag) = row_over_background(vec![
            vec![moved.clone()],
            vec![button(50.0, true)],
            vec![button(50.0, false)],
            vec![button(50.0, true)],
            vec![button(50.0, false)],
            vec![],
        ]);
        assert!(double, "the row gets its double-click");
        assert!(!drag, "a click is not a drag");

        let (_, drag) = row_over_background(vec![
            vec![moved],
            vec![button(50.0, true)],
            vec![egui::Event::PointerMoved(egui::pos2(90.0, 40.0))],
            vec![],
        ]);
        assert!(drag, "dragging over a row still moves the window");
    }

    #[test]
    fn dragging_moves_the_window_but_not_in_fullscreen() {
        assert!(drag_requested(false));
        assert!(!drag_requested(true));
    }

    #[test]
    fn chosen_file_goes_into_its_settings_field() {
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        let ctx = egui::Context::default();
        app.settings = Some(draft(&app));
        let (tx, rx) = std::sync::mpsc::channel();
        app.file_pick = Some(Pick::from_receiver(PickTarget::Epg, rx));

        app.update_file_pick(&ctx);
        assert!(app.settings.as_ref().unwrap().picking);
        tx.send(Some("/data/guide.xml.gz".into())).unwrap();
        app.update_file_pick(&ctx);
        let d = app.settings.as_ref().unwrap();
        assert_eq!(d.epg, "/data/guide.xml.gz");
        assert!(d.playlist.is_empty() && !d.picking);
        assert!(app.file_pick.is_none());
    }

    #[test]
    fn clearing_the_cache_empties_logos_and_saved_sources() {
        let root = std::env::temp_dir().join(format!("openzap-app-cache-{}", std::process::id()));
        for (dir, file) in [("logos", "a.img"), ("sources", "b.epg")] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join(file), [0u8; 10]).unwrap();
        }
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.cache_root = Some(root.clone());
        app.actions.push(Action::OpenSettings);
        app.apply_actions(&egui::Context::default());
        assert_eq!(app.settings.as_ref().unwrap().cache_size, Some(20));

        app.settings.as_mut().unwrap().clear_cache = true;
        app.update_cache_request();
        let draft = app.settings.as_ref().unwrap();
        assert_eq!(draft.cache_size, Some(0));
        assert!(!draft.clear_cache, "done once");
        assert!(app.notices.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn closing_settings_drops_the_dialog_result() {
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.file_pick = Some(Pick::from_receiver(PickTarget::Playlist, rx));
        app.update_file_pick(&egui::Context::default());
        assert!(app.file_pick.is_none());
    }

    /// Checks every second from `from` to `to` (inclusive), as frames do.
    fn wait(watch: &mut StallWatch, from: f64, to: f64, waiting: bool) -> bool {
        let mut fired = false;
        let mut t = from;
        while t <= to {
            fired |= watch.timed_out(t, waiting);
            t += 1.0;
        }
        fired
    }

    #[test]
    fn stall_watch_needs_an_unbroken_wait() {
        let mut watch = StallWatch::default();
        assert!(!wait(&mut watch, 0.0, STALL_SECS - 1.0, true));
        // A frame in between starts the count again.
        assert!(!watch.timed_out(STALL_SECS, false));
        assert!(!wait(&mut watch, STALL_SECS + 1.0, 2.0 * STALL_SECS, true));
        assert!(watch.timed_out(2.0 * STALL_SECS + 1.0, true));
        // Reported once, then counting starts over.
        assert!(!watch.timed_out(2.0 * STALL_SECS + 2.0, true));
    }

    #[test]
    fn stall_watch_ignores_time_the_window_was_not_drawing() {
        let mut watch = StallWatch::default();
        assert!(!watch.timed_out(0.0, true));
        // Minimized for a minute: no frames could arrive meanwhile.
        assert!(!watch.timed_out(60.0, true));
        assert!(!wait(&mut watch, 61.0, 60.0 + STALL_SECS - 1.0, true));
        assert!(watch.timed_out(60.0 + STALL_SECS, true));
    }

    #[test]
    fn stalled_stream_fails_and_r_restarts_it() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.playing = Some(1);
        app.current = Some(1);
        app.stream = StreamState::Opening;
        for t in 0..=STALL_SECS as u32 {
            app.check_stall(f64::from(t));
        }
        assert_eq!(app.stream, StreamState::Failed(STALL_MESSAGE.into()));
        assert_eq!(app.playing, None);
        // The stop it causes must not hide the message.
        app.apply_event(PlayerEvent::Ended);
        assert!(matches!(app.stream, StreamState::Failed(_)));

        app.restart();
        assert_eq!(app.pending_play, Some(1));
    }

    #[test]
    fn idle_and_failed_streams_are_not_watched() {
        let mut app = App::new(Playlist::default(), Epg::default(), vec![]);
        app.check_stall(0.0);
        app.check_stall(STALL_SECS * 3.0);
        assert_eq!(app.stream, StreamState::Idle);
        app.restart();
        assert_eq!(app.pending_play, None, "nothing to restart");
    }

    #[test]
    fn digits_are_taken_after_a_pause_or_on_enter() {
        let mut d = DigitEntry::default();
        assert_eq!(d.finish(0.0, true), None, "nothing typed");
        d.push('1', 0.0);
        d.push('2', 1.0);
        assert_eq!(d.text(), Some("12"));
        assert_eq!(d.finish(1.0 + DIGIT_PAUSE_SECS - 0.1, false), None);
        assert_eq!(d.finish(1.0 + DIGIT_PAUSE_SECS, false), Some(12));
        assert_eq!(d.text(), None);

        d.push('4', 5.0);
        d.push('2', 5.1);
        assert!(d.back(5.2));
        assert_eq!(d.finish(5.3, true), Some(4), "Enter takes it right away");
        assert!(!d.back(6.0), "nothing left to delete");
        for _ in 0..10 {
            d.push('9', 7.0);
        }
        assert_eq!(d.text(), Some("99999"), "capped");
        assert!(d.cancel() && !d.cancel());
    }

    #[test]
    fn typed_number_plays_that_channel_or_says_it_is_missing() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.jump_to_number(3, 0.0);
        assert_eq!((app.selected, app.pending_play), (Some(2), Some(2)));
        assert_eq!(app.scroll_to, Some(2));
        app.pending_play = None;
        app.jump_to_number(42, 0.0);
        assert_eq!(app.pending_play, None);
        assert_eq!(app.osd.map(|(t, _)| t).as_deref(), Some("No channel 42"));
    }

    #[test]
    fn backspace_returns_to_the_previous_channel() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.go_back();
        assert_eq!(app.pending_play, None, "no previous channel yet");
        // What `play` records (it needs mpv, so set it up directly).
        app.previous = Some(0);
        app.current = Some(2);
        app.go_back();
        assert_eq!((app.selected, app.pending_play), (Some(0), Some(0)));
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
    fn subtitle_selection_updates_the_list_and_osd() {
        let (playlist, epg) = demo_data(0);
        let mut app = App::new(playlist, epg, vec![]);
        app.next_subtitle(0.0);
        assert_eq!(
            app.osd.as_ref().map(|(t, _)| t.as_str()),
            Some("No subtitles")
        );

        let track = |id, lang: &str| Track {
            id,
            lang: Some(lang.into()),
            title: None,
            codec: None,
            channels: None,
            selected: false,
        };
        app.subtitles = vec![track(1, "eng"), track(2, "rus")];
        app.select_subtitle(Some(2), 1.0);
        assert!(app.subtitles[1].selected && !app.subtitles[0].selected);
        assert_eq!(
            app.osd.as_ref().map(|(t, _)| t.as_str()),
            Some("Subtitles: Russian")
        );
        app.actions.push(Action::SetSubtitle(None));
        app.apply_actions(&egui::Context::default());
        assert!(app.subtitles.iter().all(|t| !t.selected));
        assert_eq!(
            app.osd.as_ref().map(|(t, _)| t.as_str()),
            Some("Subtitles: off")
        );
    }

    #[test]
    fn subtitles_rise_above_the_bar() {
        assert_eq!(subtitle_position(0.0), 100);
        assert_eq!(subtitle_position(0.25), 75);
        assert_eq!(subtitle_position(0.9), 50, "never above the middle");
        assert_eq!(subtitle_position(-1.0), 100);
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
    fn silence_note_names_the_reason() {
        assert_eq!(silence_note(false, 80.0, false), None);
        assert_eq!(silence_note(true, 80.0, false), Some("🔇 Muted (M)"));
        assert_eq!(silence_note(false, 0.0, false), Some("🔇 Volume 0%"));
        assert_eq!(
            silence_note(false, 80.0, true),
            Some("🔇 No sound on this channel")
        );
        // Mute is the first thing to undo.
        assert_eq!(silence_note(true, 0.0, true), Some("🔇 Muted (M)"));
    }

    #[test]
    fn pressed_keys_are_listed_for_the_log() {
        let key = |key, pressed, repeat, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers,
        };
        let none = egui::Modifiers::NONE;
        let events = [
            key(egui::Key::M, true, false, none),
            key(egui::Key::M, false, false, none), // release
            key(egui::Key::ArrowUp, true, true, none), // auto-repeat
            key(egui::Key::A, true, false, egui::Modifiers::SHIFT),
            egui::Event::Text("m".into()),
        ];
        let line = pressed_keys_line(&events).unwrap();
        assert!(line.starts_with("keys: M "), "{line}");
        assert!(line.ends_with("+A"), "{line}");
        assert!(!line.contains("Up"), "{line}");
        assert_eq!(pressed_keys_line(&[egui::Event::Text("m".into())]), None);
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
