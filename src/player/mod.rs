//! Thin safe wrapper over libmpv's C client API.
//!
//! libmpv is loaded at run time (`dlopen` / `LoadLibrary`), first from the
//! directory of the executable (the library shipped with the app), then
//! from the system. So the program builds without libmpv installed and, if
//! it is missing, reports that instead of failing to start.
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
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::OnceLock;

use libloading::Library;

/// Raw C types and constants.
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
    pub const EVENT_SHUTDOWN: c_int = 1;
    pub const EVENT_START_FILE: c_int = 6;
    pub const EVENT_END_FILE: c_int = 7;
    pub const EVENT_FILE_LOADED: c_int = 8;

    pub const END_FILE_REASON_ERROR: c_int = 4;
    pub const END_FILE_REASON_REDIRECT: c_int = 5;
}

/// Declares the libmpv functions we use. Generates the `Api` struct with
/// one function pointer per entry and a loader that looks each one up by
/// name in the shared library.
macro_rules! mpv_api {
    ($( fn $name:ident($($arg:ty),*) $(-> $ret:ty)?; )*) => {
        struct Api {
            $( $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
            /// Keeps the library mapped for as long as the pointers above are used.
            _lib: Library,
        }

        impl Api {
            fn from_library(lib: Library) -> Result<Self, libloading::Error> {
                Ok(Api {
                    $(
                        $name: {
                            // SAFETY: the declared signature matches libmpv's C header.
                            let symbol = unsafe {
                                lib.get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                                    concat!(stringify!($name), "\0").as_bytes(),
                                )
                            }?;
                            *symbol // copy the function pointer out of the borrowed Symbol
                        },
                    )*
                    _lib: lib,
                })
            }
        }
    };
}

mpv_api! {
    fn mpv_create() -> *mut sys::Handle;
    fn mpv_initialize(*mut sys::Handle) -> c_int;
    fn mpv_terminate_destroy(*mut sys::Handle);
    fn mpv_set_option_string(*mut sys::Handle, *const c_char, *const c_char) -> c_int;
    fn mpv_set_property_string(*mut sys::Handle, *const c_char, *const c_char) -> c_int;
    fn mpv_command(*mut sys::Handle, *mut *const c_char) -> c_int;
    fn mpv_wait_event(*mut sys::Handle, f64) -> *mut sys::Event;
    fn mpv_error_string(c_int) -> *const c_char;
}

#[cfg(target_os = "windows")]
const LIBRARY_NAMES: &[&str] = &["libmpv-2.dll", "mpv-2.dll", "libmpv.dll", "mpv.dll"];
#[cfg(target_os = "macos")]
const LIBRARY_NAMES: &[&str] = &["libmpv.2.dylib", "libmpv.dylib"];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const LIBRARY_NAMES: &[&str] = &["libmpv.so.2", "libmpv.so.1", "libmpv.so"];

#[cfg(target_os = "windows")]
const INSTALL_HINT: &str = "put libmpv-2.dll next to openzap.exe";
#[cfg(target_os = "macos")]
const INSTALL_HINT: &str = "install it, e.g. `brew install mpv`";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const INSTALL_HINT: &str = "install it, e.g. `sudo apt install libmpv2`";

/// The loaded library, shared by all players. Loaded once on first use;
/// a failure is remembered as a message.
fn api() -> Result<&'static Api, PlayerError> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load_api)
        .as_ref()
        .map_err(|msg| PlayerError::Library(msg.clone()))
}

fn load_api() -> Result<Api, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from));
    load_from(LIBRARY_NAMES, exe_dir.as_deref())
}

/// Tries each name next to the executable first, then via the system search path.
fn load_from(names: &[&str], exe_dir: Option<&Path>) -> Result<Api, String> {
    for name in names {
        let candidates = exe_dir
            .iter()
            .map(|dir| dir.join(name))
            .chain(std::iter::once(PathBuf::from(name)));
        for path in candidates {
            // SAFETY: loading a library runs its initialization code; libmpv is trusted.
            let Ok(lib) = (unsafe { Library::new(&path) }) else {
                continue;
            };
            return Api::from_library(lib)
                .map_err(|e| format!("found libmpv but it is incompatible ({e})"));
        }
    }
    Err(format!("libmpv was not found; {INSTALL_HINT}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerError {
    /// libmpv could not be loaded (missing or incompatible).
    Library(String),
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
            PlayerError::Library(msg) => write!(f, "{msg}"),
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
    /// mpv is shutting down (e.g. the user closed its window). The instance
    /// is unusable from now on: drop this `Player` and create a new one.
    Shutdown,
}

/// Owns one mpv instance. Dropping it shuts mpv down and closes its window.
///
/// Holds a raw pointer, so the compiler makes this type neither `Send`
/// nor `Sync`: it stays on the thread that created it.
pub struct Player {
    api: &'static Api,
    handle: NonNull<sys::Handle>,
}

impl Player {
    /// Creates a player with sensible defaults for live TV.
    ///
    /// Applies platform workarounds (see `platform_options`) and then any
    /// user overrides from the `OPENZAP_MPV_OPTIONS` environment variable,
    /// e.g. `OPENZAP_MPV_OPTIONS="vo=x11,hwdec=no"`.
    pub fn new() -> Result<Self, PlayerError> {
        let mut options: Vec<(String, String)> = platform_options(crate::platform::is_wsl())
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
        let api = api()?;
        // SAFETY: `mpv_create` has no preconditions; it returns NULL on failure.
        let raw = unsafe { (api.mpv_create)() };
        let handle = NonNull::new(raw).ok_or(PlayerError::Create)?;
        // From here on `Drop` frees the handle if any step below fails.
        let player = Player { api, handle };

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
        check(api, unsafe { (api.mpv_initialize)(player.raw()) })?;
        Ok(player)
    }

    fn raw(&self) -> *mut sys::Handle {
        self.handle.as_ptr()
    }

    fn set_option(&self, name: &str, value: &str) -> Result<(), PlayerError> {
        let (name, value) = (cstring(name)?, cstring(value)?);
        // SAFETY: valid handle; both pointers are NUL-terminated and outlive the call.
        check(self.api, unsafe {
            (self.api.mpv_set_option_string)(self.raw(), name.as_ptr(), value.as_ptr())
        })
    }

    fn set_property(&self, name: &str, value: &str) -> Result<(), PlayerError> {
        let (name, value) = (cstring(name)?, cstring(value)?);
        // SAFETY: as in `set_option`.
        check(self.api, unsafe {
            (self.api.mpv_set_property_string)(self.raw(), name.as_ptr(), value.as_ptr())
        })
    }

    fn command(&self, args: &[&str]) -> Result<(), PlayerError> {
        let owned: Vec<CString> = args.iter().map(|a| cstring(a)).collect::<Result<_, _>>()?;
        // mpv wants a NULL-terminated array of C strings.
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null());
        // SAFETY: valid handle; `ptrs` is NULL-terminated and `owned` keeps
        // every string alive until after the call returns.
        check(self.api, unsafe {
            (self.api.mpv_command)(self.raw(), ptrs.as_mut_ptr())
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
        loop {
            // SAFETY: valid handle. The returned pointer is never NULL and
            // stays valid until the next `mpv_wait_event` call; we copy out
            // what we need before looping again.
            let event = unsafe { &*(self.api.mpv_wait_event)(self.raw(), timeout_secs) };
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
                            return Some(PlayerEvent::Failed(error_string(self.api, end.error)));
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
        unsafe { (self.api.mpv_terminate_destroy)(self.raw()) };
    }
}

/// mpv options that work around known problems on a platform.
///
/// WSL: the default `gpu` output aborts the whole process
/// (`vo_x11_init: Assertion '!vo->x11' failed`), and Vulkan contexts start
/// "successfully" but never show a window. Plain software `x11` output is
/// the one verified to work.
fn platform_options(wsl: bool) -> &'static [(&'static str, &'static str)] {
    if wsl { &[("vo", "x11")] } else { &[] }
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
    fn missing_library_gives_actionable_error() {
        let err = load_from(
            &["definitely-not-libmpv.so"],
            Some(Path::new("/nonexistent")),
        )
        .err()
        .expect("loading must fail");
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains(INSTALL_HINT), "{err}");
    }

    #[test]
    fn library_without_mpv_symbols_is_reported_incompatible() {
        // libm exists on Linux but certainly has no `mpv_create`.
        if cfg!(target_os = "linux") {
            let err = load_from(&["libm.so.6"], None).err().expect("must fail");
            assert!(err.contains("incompatible"), "{err}");
        }
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
    fn wsl_gets_x11_output() {
        assert!(platform_options(true).contains(&("vo", "x11")));
        assert!(platform_options(false).is_empty());
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
