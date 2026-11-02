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
    /// (`vo=libmpv`) with sensible defaults for live TV.
    ///
    /// User overrides come from the `OPENZAP_MPV_OPTIONS` environment
    /// variable, e.g. `OPENZAP_MPV_OPTIONS="hwdec=auto-safe,cache-secs=10"`.
    pub fn new() -> Result<Self, PlayerError> {
        let mut options: Vec<(String, String)> = vec![("vo".into(), "libmpv".into())];
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
            ("ytdl", "no"),     // don't run youtube-dl hooks on playlist URLs
            ("idle", "yes"),    // stay alive with no file loaded
            ("osc", "no"),      // controls are drawn by the application
            ("input-default-bindings", "no"),
        ];
        for (name, value) in defaults.iter().chain(extra) {
            player.set_option(name, value)?;
        }
        // SAFETY: the handle is valid and not yet initialized.
        check(api, unsafe { (api.mpv_initialize)(player.inner.raw()) })?;
        Ok(player)
    }

    fn set_option(&self, name: &str, value: &str) -> Result<(), PlayerError> {
        let (name, value) = (cstring(name)?, cstring(value)?);
        let api = self.inner.api;
        // SAFETY: valid handle; both pointers are NUL-terminated and outlive the call.
        check(api, unsafe {
            (api.mpv_set_option_string)(self.inner.raw(), name.as_ptr(), value.as_ptr())
        })
    }

    fn set_property(&self, name: &str, value: &str) -> Result<(), PlayerError> {
        let (name, value) = (cstring(name)?, cstring(value)?);
        let api = self.inner.api;
        // SAFETY: as in `set_option`.
        check(api, unsafe {
            (api.mpv_set_property_string)(self.inner.raw(), name.as_ptr(), value.as_ptr())
        })
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

    /// Starts playing `url`, replacing whatever is playing.
    pub fn load(&self, url: &str) -> Result<(), PlayerError> {
        self.command(&["loadfile", url, "replace"])
    }

    pub fn stop(&self) -> Result<(), PlayerError> {
        self.command(&["stop"])
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
    fn stop_without_playback_is_ok() {
        require_mpv!();
        assert_eq!(headless().stop(), Ok(()));
    }
}
