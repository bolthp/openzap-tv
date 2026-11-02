//! Thin safe wrapper over libmpv's C client API.
//!
//! mpv opens and manages its own video window; we only send commands and
//! poll events. Terminal output is disabled so mpv never logs stream URLs
//! (they may contain access keys). Error messages here come from mpv's
//! generic error codes and never include the URL.
//!
//! mpv requires the C numeric locale (`LC_NUMERIC=C`), which is the default
//! unless something calls `setlocale`; if not, `Player::new` fails.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::ptr::{self, NonNull};

/// Raw FFI declarations — the only place with `extern` items.
mod sys {
    use super::*;

    /// Opaque C type: we only ever hold a pointer to it.
    #[repr(C)]
    pub struct Handle {
        _private: [u8; 0],
    }

    /// Prefix of `mpv_event`.
    #[repr(C)]
    pub struct Event {
        pub event_id: c_int,
        pub error: c_int,
        pub reply_userdata: u64,
        pub data: *mut c_void,
    }

    /// Prefix of `mpv_event_end_file` (the fields we read).
    #[repr(C)]
    pub struct EndFile {
        pub reason: c_int,
        pub error: c_int,
    }

    pub const EVENT_NONE: c_int = 0;
    pub const EVENT_START_FILE: c_int = 6;
    pub const EVENT_END_FILE: c_int = 7;
    pub const EVENT_FILE_LOADED: c_int = 8;

    pub const END_FILE_REASON_ERROR: c_int = 4;
    pub const END_FILE_REASON_REDIRECT: c_int = 5;

    #[link(name = "mpv")]
    unsafe extern "C" {
        pub fn mpv_create() -> *mut Handle;
        pub fn mpv_initialize(ctx: *mut Handle) -> c_int;
        pub fn mpv_terminate_destroy(ctx: *mut Handle);
        pub fn mpv_set_option_string(
            ctx: *mut Handle,
            name: *const c_char,
            data: *const c_char,
        ) -> c_int;
        pub fn mpv_set_property_string(
            ctx: *mut Handle,
            name: *const c_char,
            data: *const c_char,
        ) -> c_int;
        pub fn mpv_command(ctx: *mut Handle, args: *mut *const c_char) -> c_int;
        pub fn mpv_wait_event(ctx: *mut Handle, timeout: f64) -> *mut Event;
        pub fn mpv_error_string(error: c_int) -> *const c_char;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerError {
    /// `mpv_create` returned NULL (e.g. wrong numeric locale).
    Create,
    /// A string contained an interior NUL byte and cannot be passed to C.
    InvalidString,
    /// libmpv returned an error code; holds mpv's message for it.
    Mpv(String),
}

impl fmt::Display for PlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayerError::Create => write!(f, "cannot create mpv instance"),
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
}

/// Owns one mpv instance. Dropping it shuts mpv down and closes its window.
///
/// Holds a raw pointer, so the compiler makes this type neither `Send`
/// nor `Sync`: it stays on the thread that created it.
pub struct Player {
    handle: NonNull<sys::Handle>,
}

impl Player {
    /// Creates a player with sensible defaults for live TV.
    ///
    /// Applies platform workarounds (see `platform_options`) and then any
    /// user overrides from the `OPENZAP_MPV_OPTIONS` environment variable,
    /// e.g. `OPENZAP_MPV_OPTIONS="vo=x11,hwdec=no"`.
    pub fn new() -> Result<Self, PlayerError> {
        let mut options: Vec<(String, String)> = platform_options(is_wsl())
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Ok(spec) = std::env::var("OPENZAP_MPV_OPTIONS") {
            options.extend(parse_options(&spec));
        }
        let refs: Vec<(&str, &str)> = options
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        Self::with_options(&refs)
    }

    /// Like `new`, with extra mpv options (applied after the defaults).
    pub fn with_options(extra: &[(&str, &str)]) -> Result<Self, PlayerError> {
        // SAFETY: `mpv_create` has no preconditions; it returns NULL on failure.
        let raw = unsafe { sys::mpv_create() };
        let handle = NonNull::new(raw).ok_or(PlayerError::Create)?;
        // From here on `Drop` frees the handle if any step below fails.
        let player = Player { handle };

        let defaults = [
            ("terminal", "no"), // never write logs (URLs!) to the terminal
            ("config", "no"),   // ignore user's mpv.conf for predictable behaviour
            ("ytdl", "no"),     // don't run youtube-dl hooks on playlist URLs
            ("idle", "yes"),    // stay alive with no file loaded
            ("input-default-bindings", "yes"),
            ("input-vo-keyboard", "yes"),
            ("osc", "yes"),
            ("title", "OpenZap"),
        ];
        for (name, value) in defaults.iter().chain(extra) {
            player.set_option(name, value)?;
        }
        // SAFETY: the handle is valid and not yet initialized.
        check(unsafe { sys::mpv_initialize(player.raw()) })?;
        Ok(player)
    }

    fn raw(&self) -> *mut sys::Handle {
        self.handle.as_ptr()
    }

    fn set_option(&self, name: &str, value: &str) -> Result<(), PlayerError> {
        let (name, value) = (cstring(name)?, cstring(value)?);
        // SAFETY: valid handle; both pointers are NUL-terminated and outlive the call.
        check(unsafe { sys::mpv_set_option_string(self.raw(), name.as_ptr(), value.as_ptr()) })
    }

    fn set_property(&self, name: &str, value: &str) -> Result<(), PlayerError> {
        let (name, value) = (cstring(name)?, cstring(value)?);
        // SAFETY: as in `set_option`.
        check(unsafe { sys::mpv_set_property_string(self.raw(), name.as_ptr(), value.as_ptr()) })
    }

    fn command(&self, args: &[&str]) -> Result<(), PlayerError> {
        let owned: Vec<CString> = args.iter().map(|a| cstring(a)).collect::<Result<_, _>>()?;
        // mpv wants a NULL-terminated array of C strings.
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null());
        // SAFETY: valid handle; `ptrs` is NULL-terminated and `owned` keeps
        // every string alive until after the call returns.
        check(unsafe { sys::mpv_command(self.raw(), ptrs.as_mut_ptr()) })
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
        loop {
            // SAFETY: valid handle. The returned pointer is never NULL and
            // stays valid until the next `mpv_wait_event` call; we copy out
            // what we need before looping again.
            let event = unsafe { &*sys::mpv_wait_event(self.raw(), timeout_secs) };
            match event.event_id {
                sys::EVENT_NONE => return None,
                sys::EVENT_START_FILE => return Some(PlayerEvent::Loading),
                sys::EVENT_FILE_LOADED => return Some(PlayerEvent::Started),
                sys::EVENT_END_FILE if !event.data.is_null() => {
                    // SAFETY: for END_FILE events `data` points to an mpv_event_end_file.
                    let end = unsafe { &*(event.data as *const sys::EndFile) };
                    match end.reason {
                        sys::END_FILE_REASON_REDIRECT => continue,
                        sys::END_FILE_REASON_ERROR => {
                            return Some(PlayerEvent::Failed(error_string(end.error)));
                        }
                        _ => return Some(PlayerEvent::Ended),
                    }
                }
                _ => continue, // not interesting: look at the next event
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: the handle is valid and never used after this point.
        unsafe { sys::mpv_terminate_destroy(self.raw()) };
    }
}

fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
}

/// mpv options that work around known problems on a platform.
///
/// WSL: mpv's automatic OpenGL context probing aborts the whole process
/// (`vo_x11_init: Assertion '!vo->x11' failed`), so pick a Vulkan context.
fn platform_options(wsl: bool) -> &'static [(&'static str, &'static str)] {
    if wsl {
        &[("vo", "gpu"), ("gpu-context", "x11vk")]
    } else {
        &[]
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

fn check(code: c_int) -> Result<(), PlayerError> {
    if code >= 0 {
        Ok(())
    } else {
        Err(PlayerError::Mpv(error_string(code)))
    }
}

fn error_string(code: c_int) -> String {
    // SAFETY: mpv_error_string returns a pointer to a static NUL-terminated string.
    unsafe { CStr::from_ptr(sys::mpv_error_string(code)) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A player that opens no window and plays no sound.
    fn headless() -> Player {
        Player::with_options(&[("vo", "null"), ("ao", "null")]).expect("mpv should initialize")
    }

    #[test]
    fn creates_and_drops() {
        drop(headless());
    }

    #[test]
    fn rejects_nul_bytes_without_calling_mpv() {
        let p = headless();
        assert_eq!(
            p.load("http://example.com/a\0b"),
            Err(PlayerError::InvalidString)
        );
    }

    #[test]
    fn unknown_option_is_an_error() {
        let result = Player::with_options(&[("definitely-not-an-option", "1")]);
        assert!(matches!(result, Err(PlayerError::Mpv(_))));
    }

    #[test]
    fn volume_is_accepted_and_clamped() {
        let p = headless();
        assert_eq!(p.set_volume(50.0), Ok(()));
        assert_eq!(p.set_volume(1000.0), Ok(()));
    }

    #[test]
    fn missing_file_reports_failure() {
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
    fn wsl_gets_vulkan_context() {
        assert!(platform_options(true).contains(&("gpu-context", "x11vk")));
        assert!(platform_options(false).is_empty());
    }

    #[test]
    fn stop_without_playback_is_ok() {
        assert_eq!(headless().stop(), Ok(()));
    }
}
