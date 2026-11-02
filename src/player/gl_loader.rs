//! Resolving OpenGL function pointers for libmpv's render API.
//!
//! libmpv does not load the system OpenGL library itself; the host must
//! hand it a `get_proc_address` function for the *current* GL context.
//! eframe does not expose one, so this module implements it per platform.
//! It depends only on `std` and `libloading`.

use std::ffi::{CStr, c_void};

pub struct GlLoader {
    inner: imp::Loader,
}

impl GlLoader {
    /// Loads the platform's OpenGL library. Does not need a GL context yet.
    pub fn new() -> Result<Self, String> {
        imp::Loader::new().map(|inner| GlLoader { inner })
    }

    /// Address of the GL function `name`, or null if unknown.
    ///
    /// Must be called with the GL context current on this thread.
    pub fn get(&self, name: &CStr) -> *mut c_void {
        self.inner.get(name)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use libloading::Library;
    use std::ffi::c_char;
    use std::ptr;

    type WglGetProcAddress = unsafe extern "system" fn(*const c_char) -> *mut c_void;

    pub struct Loader {
        lib: Library,
        wgl_get_proc_address: WglGetProcAddress,
    }

    impl Loader {
        pub fn new() -> Result<Self, String> {
            // SAFETY: opengl32.dll is a system library.
            let lib = unsafe { Library::new("opengl32.dll") }
                .map_err(|e| format!("cannot load opengl32.dll: {e}"))?;
            // SAFETY: the signature matches wglGetProcAddress.
            let wgl_get_proc_address =
                *unsafe { lib.get::<WglGetProcAddress>(b"wglGetProcAddress\0") }
                    .map_err(|e| format!("wglGetProcAddress not found: {e}"))?;
            Ok(Loader {
                lib,
                wgl_get_proc_address,
            })
        }

        pub fn get(&self, name: &CStr) -> *mut c_void {
            // SAFETY: the GL context is current (caller's contract) and `name` is NUL-terminated.
            let p = unsafe { (self.wgl_get_proc_address)(name.as_ptr()) };
            // wglGetProcAddress signals failure with 0, 1, 2, 3 or -1.
            if !matches!(p as usize, 0..=3 | usize::MAX) {
                return p;
            }
            // OpenGL 1.1 functions are exported by opengl32.dll itself.
            // SAFETY: only the symbol's address is read, it is never called as a typed function.
            unsafe { self.lib.get::<*mut c_void>(name.to_bytes_with_nul()) }
                .map_or(ptr::null_mut(), |symbol| *symbol)
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use libloading::Library;
    use std::ptr;

    pub struct Loader {
        lib: Library,
    }

    impl Loader {
        pub fn new() -> Result<Self, String> {
            // SAFETY: a system framework.
            let lib = unsafe { Library::new("/System/Library/Frameworks/OpenGL.framework/OpenGL") }
                .map_err(|e| format!("cannot load OpenGL.framework: {e}"))?;
            Ok(Loader { lib })
        }

        pub fn get(&self, name: &CStr) -> *mut c_void {
            // SAFETY: only the symbol's address is read, it is never called as a typed function.
            unsafe { self.lib.get::<*mut c_void>(name.to_bytes_with_nul()) }
                .map_or(ptr::null_mut(), |symbol| *symbol)
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::*;
    use libloading::Library;
    use std::ffi::c_char;
    use std::ptr;

    type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
    type GetCurrentContext = unsafe extern "C" fn() -> *mut c_void;

    /// One of the two ways to get a GL context on Linux: EGL or GLX.
    struct Backend {
        get_proc_address: GetProcAddress,
        get_current_context: GetCurrentContext,
        _lib: Library,
    }

    impl Backend {
        fn load(libs: &[&str], get_proc: &[u8], get_current: &[u8]) -> Option<Backend> {
            for name in libs {
                // SAFETY: loading a system graphics library.
                let Ok(lib) = (unsafe { Library::new(*name) }) else {
                    continue;
                };
                // SAFETY: the signatures match the EGL/GLX headers.
                let symbols = unsafe {
                    (
                        lib.get::<GetProcAddress>(get_proc).map(|s| *s),
                        lib.get::<GetCurrentContext>(get_current).map(|s| *s),
                    )
                };
                if let (Ok(get_proc_address), Ok(get_current_context)) = symbols {
                    return Some(Backend {
                        get_proc_address,
                        get_current_context,
                        _lib: lib,
                    });
                }
            }
            None
        }

        fn context_is_current(&self) -> bool {
            // SAFETY: both functions only read thread-local state.
            !unsafe { (self.get_current_context)() }.is_null()
        }

        fn get(&self, name: &CStr) -> *mut c_void {
            // SAFETY: `name` is NUL-terminated.
            unsafe { (self.get_proc_address)(name.as_ptr()) }
        }
    }

    pub struct Loader {
        egl: Option<Backend>,
        glx: Option<Backend>,
    }

    impl Loader {
        pub fn new() -> Result<Self, String> {
            let egl = Backend::load(
                &["libEGL.so.1", "libEGL.so"],
                b"eglGetProcAddress\0",
                b"eglGetCurrentContext\0",
            );
            let glx = Backend::load(
                &["libGL.so.1", "libGL.so"],
                b"glXGetProcAddressARB\0",
                b"glXGetCurrentContext\0",
            );
            if egl.is_none() && glx.is_none() {
                return Err("neither libEGL nor libGL could be loaded".into());
            }
            Ok(Loader { egl, glx })
        }

        pub fn get(&self, name: &CStr) -> *mut c_void {
            let backends = [self.egl.as_ref(), self.glx.as_ref()];
            // Prefer the API the current context was created with (EGL or GLX),
            // then fall back to the other one.
            let current = backends.iter().flatten().filter(|b| b.context_is_current());
            let others = backends
                .iter()
                .flatten()
                .filter(|b| !b.context_is_current());
            for backend in current.chain(others) {
                let p = backend.get(name);
                if !p.is_null() {
                    return p;
                }
            }
            ptr::null_mut()
        }
    }
}
