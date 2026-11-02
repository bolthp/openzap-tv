//! Run-time loading of libmpv and the raw C declarations.
//!
//! libmpv is looked up first in the directory of the executable (the
//! library shipped with the app), then in the system. So the program
//! builds without libmpv installed and, if it is missing, reports that
//! instead of failing to start.

use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libloading::Library;

use super::PlayerError;

/// Raw C types and constants.
pub(super) mod sys {
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

    /// Opaque `mpv_render_context`.
    #[repr(C)]
    pub struct RenderRaw {
        _private: [u8; 0],
    }

    /// `mpv_render_param`: a type tag and a pointer to its payload.
    #[repr(C)]
    pub struct RenderParam {
        pub kind: c_int,
        pub data: *mut c_void,
    }

    /// `mpv_opengl_init_params`.
    #[repr(C)]
    pub struct OpenGlInitParams {
        pub get_proc_address:
            Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
        pub get_proc_address_ctx: *mut c_void,
    }

    /// `mpv_opengl_fbo`.
    #[repr(C)]
    pub struct OpenGlFbo {
        pub fbo: c_int,
        pub w: c_int,
        pub h: c_int,
        pub internal_format: c_int,
    }

    pub const RENDER_PARAM_INVALID: c_int = 0;
    pub const RENDER_PARAM_API_TYPE: c_int = 1;
    pub const RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
    pub const RENDER_PARAM_OPENGL_FBO: c_int = 3;
    pub const RENDER_PARAM_FLIP_Y: c_int = 4;
    pub const RENDER_UPDATE_FRAME: u64 = 1;

    /// `mpv_event_property`.
    #[repr(C)]
    pub struct EventProperty {
        pub name: *const c_char,
        pub format: c_int,
        pub data: *mut c_void,
    }

    pub const FORMAT_FLAG: c_int = 3;
    pub const FORMAT_INT64: c_int = 4;
    pub const EVENT_PROPERTY_CHANGE: c_int = 22;

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
        pub(super) struct Api {
            $( pub(super) $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
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
    fn mpv_observe_property(*mut sys::Handle, u64, *const c_char, c_int) -> c_int;
    fn mpv_error_string(c_int) -> *const c_char;
    fn mpv_render_context_create(*mut *mut sys::RenderRaw, *mut sys::Handle, *mut sys::RenderParam) -> c_int;
    fn mpv_render_context_set_update_callback(
        *mut sys::RenderRaw,
        Option<unsafe extern "C" fn(*mut c_void)>,
        *mut c_void
    );
    fn mpv_render_context_update(*mut sys::RenderRaw) -> u64;
    fn mpv_render_context_render(*mut sys::RenderRaw, *mut sys::RenderParam) -> c_int;
    fn mpv_render_context_report_swap(*mut sys::RenderRaw);
    fn mpv_render_context_free(*mut sys::RenderRaw);
}

#[cfg(target_os = "windows")]
const LIBRARY_NAMES: &[&str] = &["libmpv-2.dll", "mpv-2.dll", "libmpv.dll", "mpv.dll"];
#[cfg(target_os = "macos")]
const LIBRARY_NAMES: &[&str] = &["libmpv.2.dylib", "libmpv.dylib"];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const LIBRARY_NAMES: &[&str] = &["libmpv.so.2", "libmpv.so.1", "libmpv.so"];

#[cfg(target_os = "windows")]
pub(super) const INSTALL_HINT: &str = "put libmpv-2.dll next to openzap.exe";
#[cfg(target_os = "macos")]
pub(super) const INSTALL_HINT: &str = "install it, e.g. `brew install mpv`";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(super) const INSTALL_HINT: &str = "install it, e.g. `sudo apt install libmpv2`";

/// The loaded library, shared by all players. Loaded once on first use;
/// a failure is remembered as a message.
pub(super) fn api() -> Result<&'static Api, PlayerError> {
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
pub(super) fn load_from(names: &[&str], exe_dir: Option<&Path>) -> Result<Api, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
