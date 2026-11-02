//! Rendering mpv video into the application's OpenGL context.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::{self, NonNull};
use std::sync::Arc;

use super::ffi::sys;
use super::gl_loader::GlLoader;
use super::{Inner, Player, PlayerError, check};

type UpdateCallback = Box<dyn Fn() + Send + Sync>;

/// mpv's OpenGL renderer for one [`Player`].
///
/// Create it (and drop it) with the GL context current. It keeps the
/// player's mpv instance alive, so the renderer is always freed before the
/// instance is destroyed, whatever the drop order.
pub struct RenderContext {
    inner: Arc<Inner>,
    raw: NonNull<sys::RenderRaw>,
    // mpv holds raw pointers to these two: they must stay alive, at a
    // stable (heap) address, for as long as `raw` exists.
    _loader: Box<GlLoader>,
    _on_update: Box<UpdateCallback>,
}

// SAFETY: the render API may be used from any single thread at a time as
// long as the GL context is current there; callers serialize access with a
// `Mutex`. The raw pointers inside are owned exclusively by this struct.
unsafe impl Send for RenderContext {}

/// Called by mpv (from an arbitrary thread) when a new frame is ready.
unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    // SAFETY: `ctx` is the address of the `UpdateCallback` stored in the
    // `RenderContext`, which outlives the registration.
    let callback = unsafe { &*(ctx as *const UpdateCallback) };
    callback();
}

/// Called by mpv while the render context is created.
unsafe extern "C" fn get_proc_address(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    // SAFETY: `ctx` is the `GlLoader` we passed; `name` is NUL-terminated.
    let loader = unsafe { &*(ctx as *const GlLoader) };
    loader.get(unsafe { CStr::from_ptr(name) })
}

impl RenderContext {
    /// Sets up OpenGL rendering for `player`. The player must have been
    /// created with `vo=libmpv` (the default of `Player::new`).
    ///
    /// The GL context must be current on this thread. `on_update` is called
    /// from an arbitrary thread whenever a new frame is available; it should
    /// only wake up the UI (e.g. request a repaint).
    pub fn new(
        player: &Player,
        on_update: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, PlayerError> {
        let inner = Arc::clone(&player.inner);
        let api = inner.api;
        let loader = Box::new(GlLoader::new().map_err(PlayerError::OpenGl)?);

        let mut init = sys::OpenGlInitParams {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx: &*loader as *const GlLoader as *mut c_void,
        };
        let mut params = [
            sys::RenderParam {
                kind: sys::RENDER_PARAM_API_TYPE,
                data: c"opengl".as_ptr() as *mut c_void,
            },
            sys::RenderParam {
                kind: sys::RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut init as *mut _ as *mut c_void,
            },
            sys::RenderParam {
                kind: sys::RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let mut raw = ptr::null_mut();
        // SAFETY: valid handle; `params` is INVALID-terminated and `init`,
        // `loader` outlive the call. The GL context is current (caller's contract).
        check(api, unsafe {
            (api.mpv_render_context_create)(&mut raw, inner.raw(), params.as_mut_ptr())
        })?;
        let raw = NonNull::new(raw).ok_or(PlayerError::Create)?;

        // Double box: `Box<dyn Fn>` is a fat pointer, but C needs a thin one.
        let on_update: Box<UpdateCallback> = Box::new(Box::new(on_update));
        // SAFETY: `on_update` lives (at this address) as long as the callback is set;
        // `Drop` unregisters it first.
        unsafe {
            (api.mpv_render_context_set_update_callback)(
                raw.as_ptr(),
                Some(update_trampoline),
                &*on_update as *const UpdateCallback as *mut c_void,
            );
        }
        Ok(RenderContext {
            inner,
            raw,
            _loader: loader,
            _on_update: on_update,
        })
    }

    /// Lets mpv process pending work; returns true if a new frame must be
    /// drawn with [`render`](Self::render). Call it on the GL thread.
    pub fn update(&self) -> bool {
        // SAFETY: valid render context.
        let flags = unsafe { (self.inner.api.mpv_render_context_update)(self.raw.as_ptr()) };
        flags & sys::RENDER_UPDATE_FRAME != 0
    }

    /// Draws the current frame into the framebuffer object `fbo` (whose size
    /// is `width` x `height` pixels, color format `internal_format`, e.g.
    /// `GL_RGBA8`; 0 if unknown), oriented for OpenGL's bottom-left origin.
    ///
    /// The GL context must be current.
    pub fn render(
        &self,
        fbo: u32,
        width: i32,
        height: i32,
        internal_format: i32,
    ) -> Result<(), PlayerError> {
        let api = self.inner.api;
        let mut target = sys::OpenGlFbo {
            fbo: fbo as c_int,
            w: width,
            h: height,
            internal_format,
        };
        let mut flip_y: c_int = 1;
        let mut params = [
            sys::RenderParam {
                kind: sys::RENDER_PARAM_OPENGL_FBO,
                data: &mut target as *mut _ as *mut c_void,
            },
            sys::RenderParam {
                kind: sys::RENDER_PARAM_FLIP_Y,
                data: &mut flip_y as *mut _ as *mut c_void,
            },
            sys::RenderParam {
                kind: sys::RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        // SAFETY: valid render context; `params` is INVALID-terminated and
        // its payloads outlive the call.
        check(api, unsafe {
            (api.mpv_render_context_render)(self.raw.as_ptr(), params.as_mut_ptr())
        })
    }

    /// Tells mpv that a frame was presented (helps its timing).
    pub fn report_swap(&self) {
        // SAFETY: valid render context.
        unsafe { (self.inner.api.mpv_render_context_report_swap)(self.raw.as_ptr()) };
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        let api = self.inner.api;
        // SAFETY: unregister the callback first so mpv can't call into the
        // box that is about to be freed, then free the context. mpv itself
        // is destroyed later, when the last `Arc<Inner>` goes away.
        unsafe {
            (api.mpv_render_context_set_update_callback)(self.raw.as_ptr(), None, ptr::null_mut());
            (api.mpv_render_context_free)(self.raw.as_ptr());
        }
    }
}
