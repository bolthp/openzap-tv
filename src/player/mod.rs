//! Thin safe wrapper over libmpv's C client API.
//!
//! libmpv is loaded at run time (see `ffi`), so the program builds without
//! it and reports a missing library instead of failing to start.
//!
//! By default the video is rendered *into the application's own OpenGL
//! context* (`vo=libmpv`): create a [`RenderContext`] for the player, then
//! draw frames with it. Terminal output is disabled so mpv never logs stream
//! URLs (they may contain access keys). Error messages here come from mpv's
//! generic error codes and never include the URL.
//!
//! mpv requires the C numeric locale (`LC_NUMERIC=C`), which is the default
//! unless something calls `setlocale`; if not, `Player::new` fails.

use std::ffi::{CStr, CString, c_char, c_int};
use std::fmt;
use std::ptr::{self, NonNull};
use std::sync::Arc;

mod ffi;
mod gl_loader;
mod render;

use ffi::{Api, api, sys};
pub use render::RenderContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerError {
    /// libmpv could not be loaded (missing or incompatible).
    Library(String),
    /// `mpv_create` returned NULL (e.g. wrong numeric locale).
    Create,
    /// OpenGL function loading or setup failed (video embedding).
    OpenGl(String),
    /// A string contained an interior NUL byte and cannot be passed to C.
    InvalidString,
    /// libmpv returned an error code; holds mpv's message for it.
    Mpv(String),
}

impl fmt::Display for PlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayerError::Library(msg) => write!(f, "{msg}"),
            PlayerError::Create => write!(f, "cannot create mpv instance"),
            PlayerError::OpenGl(msg) => write!(f, "OpenGL: {msg}"),
            PlayerError::InvalidString => write!(f, "string contains a NUL byte"),
            PlayerError::Mpv(msg) => write!(f, "mpv: {msg}"),
        }
    }
}

impl std::error::Error for PlayerError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// mpv started opening a stream.
    Loading,
    /// The stream is open and playback begins.
    Started,
    /// Playback ended normally or was stopped.
    Ended,
    /// Playback failed; the message is mpv's generic error text.
    Failed(String),
    /// mpv is shutting down. The instance is unusable from now on: drop this
    /// `Player` and create a new one.
    Shutdown,
    /// A message from mpv's log (see [`Player::request_log_messages`]).
    /// The text may contain stream URLs: mask it before writing it anywhere.
    Log {
        prefix: String,
        level: String,
        text: String,
    },
    /// An observed property changed (see [`Player::observe`]).
    Property { name: String, value: PropertyValue },
}

/// Value type to observe a property as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKind {
    Flag,
    Int,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyValue {
    Flag(bool),
    Int(i64),
    /// The property is currently unavailable (e.g. nothing is playing).
    Unavailable,
}

/// How video is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HardwareDecoding {
    /// Always decode on the CPU.
    Off,
    /// Use the GPU when mpv considers it safe, fall back to the CPU otherwise
    /// (verified on Windows/NVIDIA: nvdec, also for 4K HEVC).
    #[default]
    Auto,
}

impl HardwareDecoding {
    fn mpv_value(self) -> &'static str {
        match self {
            HardwareDecoding::Off => "no",
            HardwareDecoding::Auto => "auto-safe",
        }
    }
}

/// How the picture is fitted into the video area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AspectMode {
    /// The stream's own aspect ratio, letterboxed.
    #[default]
    Auto,
    Ratio16x9,
    Ratio4x3,
    /// Keep the aspect ratio, crop the edges to fill the area.
    Fill,
    /// Ignore the aspect ratio, stretch to the area.
    Stretch,
}

impl AspectMode {
    pub const ALL: [AspectMode; 5] = [
        AspectMode::Auto,
        AspectMode::Ratio16x9,
        AspectMode::Ratio4x3,
        AspectMode::Fill,
        AspectMode::Stretch,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AspectMode::Auto => "Auto",
            AspectMode::Ratio16x9 => "16:9",
            AspectMode::Ratio4x3 => "4:3",
            AspectMode::Fill => "Fill (crop)",
            AspectMode::Stretch => "Stretch",
        }
    }

    /// The next mode, wrapping around (for a cycle key).
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|&m| m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// The mpv properties for this mode; every mode sets all of them, so
    /// switching never leaves a previous mode's setting behind.
    fn mpv_properties(self) -> [(&'static str, &'static str); 3] {
        let (aspect, panscan, keep) = match self {
            AspectMode::Auto => ("no", "0", "yes"),
            AspectMode::Ratio16x9 => ("16:9", "0", "yes"),
            AspectMode::Ratio4x3 => ("4:3", "0", "yes"),
            AspectMode::Fill => ("no", "1", "yes"),
            AspectMode::Stretch => ("no", "0", "no"),
        };
        [
            ("video-aspect-override", aspect),
            ("panscan", panscan),
            ("keepaspect", keep),
        ]
    }
}

/// Playback options the user can change; applied at creation and live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackSettings {
    pub hwdec: HardwareDecoding,
    /// How many seconds of a stream to read ahead (network buffer).
    pub buffer_secs: u32,
}

impl PlaybackSettings {
    pub const MIN_BUFFER_SECS: u32 = 1;
    pub const MAX_BUFFER_SECS: u32 = 120;

    /// The mpv options these settings stand for.
    fn mpv_options(&self) -> Vec<(&'static str, String)> {
        let secs = self
            .buffer_secs
            .clamp(Self::MIN_BUFFER_SECS, Self::MAX_BUFFER_SECS)
            .to_string();
        vec![
            ("hwdec", self.hwdec.mpv_value().to_string()),
            ("cache-secs", secs.clone()),
            ("demuxer-readahead-secs", secs),
        ]
    }
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        PlaybackSettings {
            hwdec: HardwareDecoding::default(),
            buffer_secs: 10,
        }
    }
}

/// The mpv instance itself. Shared (`Arc`) between the [`Player`] and any
/// [`RenderContext`], so it is destroyed only after the last user is gone.
struct Inner {
    api: &'static Api,
    handle: NonNull<sys::Handle>,
}

// SAFETY: libmpv's client API is documented as thread-safe: all functions
// may be called from any thread.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Inner {
    fn raw(&self) -> *mut sys::Handle {
        self.handle.as_ptr()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: the handle is valid and never used after this point.
        // Any render context holds an `Arc<Inner>`, so it is already freed.
        unsafe { (self.api.mpv_terminate_destroy)(self.raw()) };
    }
}

/// Controls one mpv instance. Dropping the last handle shuts mpv down.
pub struct Player {
    inner: Arc<Inner>,
}

impl Player {
    /// Creates a player that renders into the application's OpenGL context
    /// (`vo=libmpv`) with sensible defaults for live TV and `settings`.
    ///
    /// Developer overrides come last, from the `OPENZAP_MPV_OPTIONS`
    /// environment variable, e.g. `OPENZAP_MPV_OPTIONS="log-file=mpv.log"`.
    pub fn new(settings: &PlaybackSettings) -> Result<Self, PlayerError> {
        let mut options: Vec<(String, String)> = vec![("vo".into(), "libmpv".into())];
        options.extend(
            settings
                .mpv_options()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v)),
        );
        if let Ok(spec) = std::env::var("OPENZAP_MPV_OPTIONS") {
            options.extend(parse_options(&spec));
        }
        let refs: Vec<(&str, &str)> = options
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        Self::with_options(&refs)
    }

    /// Like `new`, but with exactly the given extra mpv options (applied
    /// after the defaults) and no `vo` choice.
    pub fn with_options(extra: &[(&str, &str)]) -> Result<Self, PlayerError> {
        let api = api()?;
        // SAFETY: `mpv_create` has no preconditions; it returns NULL on failure.
        let raw = unsafe { (api.mpv_create)() };
        let handle = NonNull::new(raw).ok_or(PlayerError::Create)?;
        // From here on `Drop` frees the handle if any step below fails.
        let player = Player {
            inner: Arc::new(Inner { api, handle }),
        };

        let defaults = [
            ("terminal", "no"), // never write logs (URLs!) to the terminal
            ("config", "no"),   // ignore user's mpv.conf for predictable behaviour
            ("idle", "yes"),    // stay alive with no file loaded
            ("input-default-bindings", "no"),
        ];
        // Only builds with Lua scripts have these (e.g. not the Flatpak's):
        // without them there is nothing to turn off.
        let script_options = [
            ("ytdl", "no"), // don't run youtube-dl hooks on playlist URLs
            ("osc", "no"),  // controls are drawn by the application
        ];
        for (name, value) in script_options {
            player.set_optional_option(name, value)?;
        }
        for (name, value) in defaults.iter().chain(extra) {
            player.set_option(name, value)?;
        }
        // SAFETY: the handle is valid and not yet initialized.
        check(api, unsafe { (api.mpv_initialize)(player.inner.raw()) })?;
        Ok(player)
    }

    fn set_option(&self, name: &str, value: &str) -> Result<(), PlayerError> {
        check(self.inner.api, self.set_option_code(name, value)?)
    }

    /// Like `set_option`, but an option this libmpv build does not have is
    /// skipped (a bad value for one it has is still an error).
    fn set_optional_option(&self, name: &str, value: &str) -> Result<(), PlayerError> {
        match self.set_option_code(name, value)? {
            sys::ERROR_OPTION_NOT_FOUND => Ok(()),
            code => check(self.inner.api, code),
        }
    }

    /// Sets an option; returns mpv's status code.
    fn set_option_code(&self, name: &str, value: &str) -> Result<c_int, PlayerError> {
        let (name, value) = (cstring(name)?, cstring(value)?);
        let api = self.inner.api;
        // SAFETY: valid handle; both pointers are NUL-terminated and outlive the call.
        Ok(unsafe { (api.mpv_set_option_string)(self.inner.raw(), name.as_ptr(), value.as_ptr()) })
    }

    fn set_property(&self, name: &str, value: &str) -> Result<(), PlayerError> {
        let (name, value) = (cstring(name)?, cstring(value)?);
        let api = self.inner.api;
        // SAFETY: as in `set_option`.
        check(api, unsafe {
            (api.mpv_set_property_string)(self.inner.raw(), name.as_ptr(), value.as_ptr())
        })
    }

    /// Reads a property as text; `None` if it is unavailable right now
    /// (e.g. no video yet) or unknown.
    pub fn get_property(&self, name: &str) -> Option<String> {
        let name = cstring(name).ok()?;
        let api = self.inner.api;
        // SAFETY: valid handle; `name` is NUL-terminated. A non-NULL result
        // is a NUL-terminated string owned by us, freed with `mpv_free`.
        unsafe {
            let raw = (api.mpv_get_property_string)(self.inner.raw(), name.as_ptr());
            if raw.is_null() {
                return None;
            }
            let value = CStr::from_ptr(raw).to_string_lossy().into_owned();
            (api.mpv_free)(raw.cast());
            Some(value)
        }
    }

    fn command(&self, args: &[&str]) -> Result<(), PlayerError> {
        let owned: Vec<CString> = args.iter().map(|a| cstring(a)).collect::<Result<_, _>>()?;
        // mpv wants a NULL-terminated array of C strings.
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null());
        let api = self.inner.api;
        // SAFETY: valid handle; `ptrs` is NULL-terminated and `owned` keeps
        // every string alive until after the call returns.
        check(api, unsafe {
            (api.mpv_command)(self.inner.raw(), ptrs.as_mut_ptr())
        })
    }

    /// Starts playing `url`, replacing whatever is playing. Audio and
    /// subtitle choices made for the previous stream are dropped: mpv
    /// would otherwise ask the new stream for the same track ids.
    pub fn load(&self, url: &str) -> Result<(), PlayerError> {
        self.set_property("aid", "auto")?;
        self.set_property("sid", "auto")?;
        self.command(&["loadfile", url, "replace"])
    }

    pub fn stop(&self) -> Result<(), PlayerError> {
        self.command(&["stop"])
    }

    pub fn set_mute(&self, mute: bool) -> Result<(), PlayerError> {
        self.set_property("mute", if mute { "yes" } else { "no" })
    }

    /// Asks mpv to report changes of `name` as [`PlayerEvent::Property`].
    /// The current value is reported right away.
    pub fn observe(&self, name: &str, kind: PropertyKind) -> Result<(), PlayerError> {
        let name = cstring(name)?;
        let format = match kind {
            PropertyKind::Flag => sys::FORMAT_FLAG,
            PropertyKind::Int => sys::FORMAT_INT64,
        };
        let api = self.inner.api;
        // SAFETY: valid handle; `name` is NUL-terminated (mpv copies it).
        check(api, unsafe {
            (api.mpv_observe_property)(self.inner.raw(), 0, name.as_ptr(), format)
        })
    }

    /// Delivers mpv's own log messages of `min_level` and above (`"error"`,
    /// `"warn"`, `"info"`, `"v"`, `"debug"`) as [`PlayerEvent::Log`].
    pub fn request_log_messages(&self, min_level: &str) -> Result<(), PlayerError> {
        let level = cstring(min_level)?;
        let api = self.inner.api;
        // SAFETY: valid handle; `level` is NUL-terminated.
        check(api, unsafe {
            (api.mpv_request_log_messages)(self.inner.raw(), level.as_ptr())
        })
    }

    /// Applies changed settings to the running player. Decoder changes take
    /// effect right away (mpv reinitializes the decoder).
    pub fn apply(&self, settings: &PlaybackSettings) -> Result<(), PlayerError> {
        for (name, value) in settings.mpv_options() {
            self.set_property(name, &value)?;
        }
        Ok(())
    }

    pub fn set_aspect(&self, mode: AspectMode) -> Result<(), PlayerError> {
        for (name, value) in mode.mpv_properties() {
            self.set_property(name, value)?;
        }
        Ok(())
    }

    /// Switches to the audio track with mpv's track `id`
    /// (see [`crate::tracks::audio_tracks`]).
    ///
    /// mpv reads only the selected tracks. A live stream cannot be read
    /// again from the playing position, so the new track would be heard
    /// only once playback reaches what is read after the switch: a whole
    /// network buffer of silence. Such streams are opened again instead,
    /// with the new track (about as quick as switching channels).
    pub fn set_audio_track(&self, id: i64) -> Result<(), PlayerError> {
        self.switch_audio(&id.to_string())
    }

    /// Pauses or resumes playback.
    pub fn set_pause(&self, paused: bool) -> Result<(), PlayerError> {
        self.set_property("pause", if paused { "yes" } else { "no" })
    }

    /// Plays on without sound (for audio that cannot be decoded, which
    /// would otherwise hold the picture). The next `load` turns it on again.
    pub fn disable_audio(&self) -> Result<(), PlayerError> {
        self.switch_audio("no")
    }

    fn switch_audio(&self, aid: &str) -> Result<(), PlayerError> {
        self.set_property("aid", aid)?;
        if self.get_property("seekable").as_deref() != Some("no") {
            return Ok(());
        }
        match self.get_property("path") {
            Some(path) => self.command(&["loadfile", &path, "replace"]),
            None => Ok(()),
        }
    }

    /// Shows the subtitle track with mpv's track `id`, or none
    /// (see [`crate::tracks::subtitle_tracks`]).
    pub fn set_subtitle_track(&self, id: Option<i64>) -> Result<(), PlayerError> {
        let value = id.map_or_else(|| "no".to_string(), |id| id.to_string());
        self.set_property("sid", &value)
    }

    /// Vertical subtitle position in percent of the picture height
    /// (100 = mpv's default, at the bottom).
    pub fn set_subtitle_position(&self, percent: u32) -> Result<(), PlayerError> {
        self.set_property("sub-pos", &percent.min(150).to_string())
    }

    /// Volume in percent (0–130).
    pub fn set_volume(&self, percent: f64) -> Result<(), PlayerError> {
        self.set_property("volume", &percent.clamp(0.0, 130.0).to_string())
    }

    /// Returns the next interesting event without waiting, if any.
    pub fn poll(&self) -> Option<PlayerEvent> {
        self.wait(0.0)
    }

    /// Waits up to `timeout_secs` for the next interesting event.
    pub fn wait(&self, timeout_secs: f64) -> Option<PlayerEvent> {
        let api = self.inner.api;
        loop {
            // SAFETY: valid handle. The returned pointer is never NULL and
            // stays valid until the next `mpv_wait_event` call; we copy out
            // what we need before looping again.
            let event = unsafe { &*(api.mpv_wait_event)(self.inner.raw(), timeout_secs) };
            match event.event_id {
                sys::EVENT_NONE => return None,
                sys::EVENT_SHUTDOWN => return Some(PlayerEvent::Shutdown),
                sys::EVENT_START_FILE => return Some(PlayerEvent::Loading),
                sys::EVENT_FILE_LOADED => return Some(PlayerEvent::Started),
                sys::EVENT_LOG_MESSAGE if !event.data.is_null() => {
                    // SAFETY: for LOG_MESSAGE events `data` points to an
                    // mpv_event_log_message with NUL-terminated strings.
                    let msg = unsafe { &*(event.data as *const sys::EventLogMessage) };
                    let text = |p: *const c_char| {
                        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
                    };
                    return Some(PlayerEvent::Log {
                        prefix: text(msg.prefix),
                        level: text(msg.level),
                        text: text(msg.text).trim_end().to_string(),
                    });
                }
                sys::EVENT_PROPERTY_CHANGE if !event.data.is_null() => {
                    // SAFETY: for PROPERTY_CHANGE events `data` points to an
                    // mpv_event_property whose `name` is NUL-terminated and
                    // whose `data` matches `format`.
                    let prop = unsafe { &*(event.data as *const sys::EventProperty) };
                    let name = unsafe { CStr::from_ptr(prop.name) }
                        .to_string_lossy()
                        .into_owned();
                    let value = match prop.format {
                        sys::FORMAT_FLAG if !prop.data.is_null() => {
                            PropertyValue::Flag(unsafe { *(prop.data as *const c_int) } != 0)
                        }
                        sys::FORMAT_INT64 if !prop.data.is_null() => {
                            PropertyValue::Int(unsafe { *(prop.data as *const i64) })
                        }
                        _ => PropertyValue::Unavailable,
                    };
                    return Some(PlayerEvent::Property { name, value });
                }
                sys::EVENT_END_FILE if !event.data.is_null() => {
                    // SAFETY: for END_FILE events `data` points to an mpv_event_end_file.
                    let end = unsafe { &*(event.data as *const sys::EndFile) };
                    match end.reason {
                        sys::END_FILE_REASON_REDIRECT => continue,
                        sys::END_FILE_REASON_ERROR => {
                            return Some(PlayerEvent::Failed(error_string(api, end.error)));
                        }
                        _ => return Some(PlayerEvent::Ended),
                    }
                }
                _ => continue, // not interesting: look at the next event
            }
        }
    }
}

/// Parses `key=value,key=value`. Entries without `=` are skipped.
fn parse_options(spec: &str) -> Vec<(String, String)> {
    spec.split(',')
        .filter_map(|entry| entry.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

fn cstring(s: &str) -> Result<CString, PlayerError> {
    CString::new(s).map_err(|_| PlayerError::InvalidString)
}

fn check(api: &Api, code: c_int) -> Result<(), PlayerError> {
    if code >= 0 {
        Ok(())
    } else {
        Err(PlayerError::Mpv(error_string(api, code)))
    }
}

fn error_string(api: &Api, code: c_int) -> String {
    // SAFETY: mpv_error_string returns a pointer to a static NUL-terminated string.
    unsafe { CStr::from_ptr((api.mpv_error_string)(code)) }
        .to_string_lossy()
        .into_owned()
}
#[cfg(test)]
mod tests {
    use super::*;

    /// True if libmpv can be loaded. Without it, mpv tests are skipped
    /// locally; set `OPENZAP_REQUIRE_MPV=1` (as CI does) to make that a failure.
    fn mpv_available() -> bool {
        match api() {
            Ok(_) => true,
            Err(e) => {
                assert!(
                    std::env::var_os("OPENZAP_REQUIRE_MPV").is_none(),
                    "libmpv is required but unavailable: {e}"
                );
                eprintln!("skipping: {e}");
                false
            }
        }
    }

    macro_rules! require_mpv {
        () => {
            if !mpv_available() {
                return;
            }
        };
    }

    /// A player that opens no window and plays no sound.
    fn headless() -> Player {
        Player::with_options(&[("vo", "null"), ("ao", "null")]).expect("mpv should initialize")
    }

    #[test]
    fn creates_and_drops() {
        require_mpv!();
        drop(headless());
    }

    #[test]
    fn rejects_nul_bytes_without_calling_mpv() {
        require_mpv!();
        let p = headless();
        assert_eq!(
            p.load("http://example.com/a\0b"),
            Err(PlayerError::InvalidString)
        );
    }

    #[test]
    fn unknown_option_is_an_error() {
        require_mpv!();
        let result = Player::with_options(&[("definitely-not-an-option", "1")]);
        assert!(matches!(result, Err(PlayerError::Mpv(_))));
    }

    #[test]
    fn optional_options_may_be_missing_but_not_wrong() {
        require_mpv!();
        let p = headless();
        assert_eq!(
            p.set_optional_option("definitely-not-an-option", "1"),
            Ok(())
        );
        assert!(matches!(
            p.set_optional_option("idle", "maybe"),
            Err(PlayerError::Mpv(_))
        ));
    }

    #[test]
    fn volume_is_accepted_and_clamped() {
        require_mpv!();
        let p = headless();
        assert_eq!(p.set_volume(50.0), Ok(()));
        assert_eq!(p.set_volume(1000.0), Ok(()));
    }

    #[test]
    fn missing_file_reports_failure() {
        require_mpv!();
        let p = headless();
        p.load("/nonexistent/openzap-test.mp4").unwrap();
        let mut failed = false;
        for _ in 0..50 {
            match p.wait(0.1) {
                Some(PlayerEvent::Failed(msg)) => {
                    assert!(!msg.is_empty());
                    failed = true;
                    break;
                }
                Some(_) => {}
                None => {}
            }
        }
        assert!(failed, "expected a Failed event");
    }

    #[test]
    fn loading_a_stream_drops_track_choices() {
        require_mpv!();
        let p = headless();
        p.set_audio_track(2).unwrap();
        p.set_subtitle_track(Some(3)).unwrap();
        p.load("/nonexistent/openzap-test.mp4").unwrap();
        assert_eq!(p.get_property("aid").as_deref(), Some("auto"));
        assert_eq!(p.get_property("sid").as_deref(), Some("auto"));
    }

    /// Serves `connections` requests with an endless-looking WAV (no
    /// length, like a live stream, so mpv cannot seek in it).
    fn serve_live_wav(connections: usize) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/live.wav", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(connections) {
                let Ok(mut stream) = stream else { continue };
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                // 8 kHz mono 8-bit PCM; sizes 0xFFFFFFFF mean "unknown".
                let mut wav = b"HTTP/1.0 200 OK\r\nContent-Type: audio/wav\r\n\r\n".to_vec();
                wav.extend_from_slice(b"RIFF\xff\xff\xff\xffWAVEfmt ");
                wav.extend_from_slice(&16u32.to_le_bytes());
                wav.extend_from_slice(&[1, 0, 1, 0]); // PCM, 1 channel
                wav.extend_from_slice(&8000u32.to_le_bytes());
                wav.extend_from_slice(&8000u32.to_le_bytes());
                wav.extend_from_slice(&[1, 0, 8, 0]); // block align, bits
                wav.extend_from_slice(b"data\xff\xff\xff\xff");
                wav.resize(wav.len() + 8000 * 30, 128); // 30 s of silence
                let _ = stream.write_all(&wav);
            }
        });
        url
    }

    /// Waits for the next event that `want` accepts.
    fn wait_for(p: &Player, want: impl Fn(&PlayerEvent) -> bool) -> bool {
        (0..100).any(|_| p.wait(0.1).is_some_and(|ev| want(&ev)))
    }

    #[test]
    fn audio_switch_reopens_a_live_stream() {
        require_mpv!();
        let p = headless();
        p.load(&serve_live_wav(2)).unwrap();
        assert!(wait_for(&p, |ev| *ev == PlayerEvent::Started));
        assert_eq!(p.get_property("seekable").as_deref(), Some("no"));

        p.set_audio_track(1).unwrap();
        assert!(wait_for(&p, |ev| *ev == PlayerEvent::Loading), "reopened");
        assert!(wait_for(&p, |ev| *ev == PlayerEvent::Started));
        assert_eq!(p.get_property("aid").as_deref(), Some("1"));
    }

    #[test]
    fn parses_env_options() {
        assert_eq!(
            parse_options("vo=x11, hwdec = no,junk,=x"),
            [
                ("vo".to_string(), "x11".to_string()),
                ("hwdec".to_string(), "no".to_string())
            ]
        );
        assert!(parse_options("").is_empty());
    }

    #[test]
    fn quit_command_produces_shutdown_event() {
        require_mpv!();
        // Closing mpv's window has the same effect as the `quit` command.
        let p = headless();
        p.command(&["quit"]).unwrap();
        let mut got = false;
        for _ in 0..50 {
            if p.wait(0.1) == Some(PlayerEvent::Shutdown) {
                got = true;
                break;
            }
        }
        assert!(got, "expected a Shutdown event");
    }

    #[test]
    fn reads_properties_as_text() {
        require_mpv!();
        let p = headless();
        p.set_volume(42.0).unwrap();
        assert_eq!(p.get_property("volume").as_deref(), Some("42.000000"));
        // Nothing is playing, so there is no video.
        assert_eq!(p.get_property("video-params/w"), None);
        assert_eq!(p.get_property("no-such-property"), None);
    }

    #[test]
    fn every_aspect_mode_is_accepted() {
        require_mpv!();
        let p = headless();
        for mode in AspectMode::ALL {
            assert_eq!(p.set_aspect(mode), Ok(()), "{mode:?}");
        }
        assert_eq!(p.get_property("keepaspect").as_deref(), Some("no"));
        p.set_aspect(AspectMode::Auto).unwrap();
        assert_eq!(p.get_property("keepaspect").as_deref(), Some("yes"));
    }

    #[test]
    fn subtitles_can_be_turned_off_without_a_file() {
        require_mpv!();
        let p = headless();
        assert_eq!(p.set_subtitle_track(None), Ok(()));
        assert_eq!(p.set_subtitle_position(80), Ok(()));
        assert_eq!(p.get_property("sub-pos").as_deref(), Some("80.000000"));
        assert_eq!(p.get_property("sid").as_deref(), Some("no"));
    }

    #[test]
    fn aspect_modes_cycle_through_all() {
        let mut mode = AspectMode::Auto;
        let mut seen = Vec::new();
        for _ in 0..AspectMode::ALL.len() {
            seen.push(mode);
            mode = mode.next();
        }
        assert_eq!(mode, AspectMode::Auto);
        assert_eq!(seen, AspectMode::ALL);
    }

    /// Waits for the next Property event about `name`.
    fn next_property(p: &Player, name: &str) -> Option<PropertyValue> {
        for _ in 0..50 {
            if let Some(PlayerEvent::Property { name: n, value }) = p.wait(0.1)
                && n == name
            {
                return Some(value);
            }
        }
        None
    }

    #[test]
    fn observed_flag_reports_initial_value_and_changes() {
        require_mpv!();
        let p = headless();
        p.observe("mute", PropertyKind::Flag).unwrap();
        assert_eq!(next_property(&p, "mute"), Some(PropertyValue::Flag(false)));
        p.set_mute(true).unwrap();
        assert_eq!(next_property(&p, "mute"), Some(PropertyValue::Flag(true)));
    }

    #[test]
    fn observed_int_property() {
        require_mpv!();
        let p = headless();
        p.set_volume(42.0).unwrap();
        p.observe("volume", PropertyKind::Int).unwrap();
        assert_eq!(next_property(&p, "volume"), Some(PropertyValue::Int(42)));
    }

    #[test]
    fn unknown_property_is_rejected_or_unavailable() {
        require_mpv!();
        let p = headless();
        // Either outcome is safe for callers; a bogus value would not be.
        if p.observe("no-such-property", PropertyKind::Flag).is_ok() {
            assert_eq!(
                next_property(&p, "no-such-property"),
                Some(PropertyValue::Unavailable)
            );
        }
    }

    #[test]
    fn log_messages_are_delivered() {
        require_mpv!();
        let p = headless();
        p.request_log_messages("v").unwrap();
        p.load("/nonexistent/openzap-log-test.mp4").unwrap();
        let mut got = false;
        for _ in 0..50 {
            if let Some(PlayerEvent::Log {
                prefix,
                level,
                text,
            }) = p.wait(0.1)
            {
                assert!(!prefix.is_empty() && !level.is_empty());
                assert!(!text.ends_with('\n'));
                got = true;
                break;
            }
        }
        assert!(got, "expected at least one log message");
    }

    #[test]
    fn settings_map_to_mpv_options() {
        let s = PlaybackSettings {
            hwdec: HardwareDecoding::Auto,
            buffer_secs: 30,
        };
        assert_eq!(
            s.mpv_options(),
            [
                ("hwdec", "auto-safe".to_string()),
                ("cache-secs", "30".to_string()),
                ("demuxer-readahead-secs", "30".to_string()),
            ]
        );
        let extreme = PlaybackSettings {
            hwdec: HardwareDecoding::Off,
            buffer_secs: 100_000,
        };
        assert_eq!(extreme.mpv_options()[0].1, "no");
        assert_eq!(extreme.mpv_options()[1].1, "120");
    }

    #[test]
    fn settings_are_accepted_by_mpv_at_creation_and_live() {
        require_mpv!();
        let p = headless();
        let auto = PlaybackSettings {
            hwdec: HardwareDecoding::Auto,
            buffer_secs: 5,
        };
        assert_eq!(p.apply(&auto), Ok(()));
        assert_eq!(p.apply(&PlaybackSettings::default()), Ok(()));

        let options: Vec<(String, String)> = auto
            .mpv_options()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .chain([
                ("vo".to_string(), "null".to_string()),
                ("ao".to_string(), "null".to_string()),
            ])
            .collect();
        let refs: Vec<(&str, &str)> = options
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert!(Player::with_options(&refs).is_ok());
    }

    #[test]
    fn stop_without_playback_is_ok() {
        require_mpv!();
        assert_eq!(headless().stop(), Ok(()));
    }
}
