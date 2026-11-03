//! Embedded video: mpv renders into an offscreen texture, which is then
//! copied into the egui frame inside a paint callback.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui_glow;
use eframe::glow::{self, HasContext};
use openzap::player::{Player, PlayerError, RenderContext};

/// The texture mpv draws into, attached to a framebuffer object.
struct Target {
    fbo: glow::Framebuffer,
    texture: glow::Texture,
    width: i32,
    height: i32,
}

/// How often diagnostics summarize rendering.
const REPORT_EVERY: Duration = Duration::from_secs(2);

/// Rendering statistics for the debug log.
#[derive(Default)]
struct Diagnostics {
    gl_info_reported: bool,
    renders: u32,
    last_report: Option<Instant>,
    reports: Vec<String>,
}

#[derive(Default)]
struct State {
    render: Option<RenderContext>,
    target: Option<Target>,
    /// The target was (re)created and holds no frame yet.
    needs_render: bool,
    /// Blank the target on the next draw (a new stream is opening, so the
    /// previous channel's last frame must not stay on screen).
    clear_pending: bool,
    /// Frames drawn since the last `clear` (did the new stream show anything?).
    frames_since_clear: u64,
    /// Last GL error from setting up the target, reported once.
    error: Option<String>,
    /// Present only when diagnostics are enabled (`--debug`).
    diagnostics: Option<Diagnostics>,
}

/// Shared handle to the embedded video. Cloning is cheap: the paint
/// callback holds a clone, the app holds the original.
#[derive(Clone, Default)]
pub struct VideoView {
    state: Arc<Mutex<State>>,
}

impl VideoView {
    /// Connects mpv's renderer to the app's GL context.
    ///
    /// Must be called with the GL context current, which holds inside
    /// `App::ui` with eframe's glow backend.
    pub fn attach(&self, player: &Player, ctx: &egui::Context) -> Result<(), PlayerError> {
        let repaint = ctx.clone();
        // mpv calls this from its own thread when a frame is ready.
        let render = RenderContext::new(player, move || repaint.request_repaint())?;
        let mut state = self.lock();
        state.render = Some(render);
        state.needs_render = true;
        Ok(())
    }

    /// Frees mpv's renderer. Call before dropping the player, with the GL
    /// context current (in `ui` or `on_exit`).
    pub fn detach(&self) {
        self.lock().render = None;
    }

    /// Starts collecting rendering statistics (see [`take_reports`](Self::take_reports)).
    pub fn enable_diagnostics(&self) {
        self.lock()
            .diagnostics
            .get_or_insert_with(Diagnostics::default);
    }

    /// Diagnostic lines collected since the last call.
    pub fn take_reports(&self) -> Vec<String> {
        self.lock()
            .diagnostics
            .as_mut()
            .map(|d| std::mem::take(&mut d.reports))
            .unwrap_or_default()
    }

    /// Blanks the picture until the next frame arrives.
    pub fn clear(&self) {
        let mut state = self.lock();
        state.clear_pending = true;
        state.frames_since_clear = 0;
    }

    /// At least one frame of the current stream has been drawn.
    pub fn has_frame(&self) -> bool {
        self.lock().frames_since_clear > 0
    }

    #[cfg(test)]
    pub fn is_clear_pending(&self) -> bool {
        self.lock().clear_pending
    }

    pub fn is_attached(&self) -> bool {
        self.lock().render.is_some()
    }

    /// Takes the last setup error, if any.
    pub fn take_error(&self) -> Option<String> {
        self.lock().error.take()
    }

    /// Schedules drawing the current video frame into `rect`.
    pub fn paint(&self, ui: &egui::Ui, rect: egui::Rect) {
        let view = self.clone();
        let callback = egui_glow::CallbackFn::new(move |info, painter| {
            view.lock().draw(painter, &info);
        });
        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(callback),
        });
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves the state usable: recover it.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl State {
    fn draw(&mut self, painter: &egui_glow::Painter, info: &egui::PaintCallbackInfo) {
        let gl = painter.gl();
        let viewport = info.viewport_in_pixels();
        let (width, height) = (viewport.width_px, viewport.height_px);
        if self.render.is_none() || width <= 0 || height <= 0 {
            return;
        }

        // SAFETY (whole block): egui_glow calls us with its GL context
        // current; every object used here was created in that context.
        unsafe {
            if self
                .target
                .as_ref()
                .is_none_or(|t| (t.width, t.height) != (width, height))
            {
                if let Some(old) = self.target.take() {
                    gl.delete_framebuffer(old.fbo);
                    gl.delete_texture(old.texture);
                }
                match create_target(gl, width, height) {
                    Ok(target) => {
                        self.target = Some(target);
                        self.needs_render = true;
                    }
                    Err(e) => {
                        self.error = Some(e);
                        return;
                    }
                }
            }
            if let Some(diag) = &mut self.diagnostics
                && !diag.gl_info_reported
            {
                diag.gl_info_reported = true;
                diag.reports.push(format!(
                    "OpenGL: {} | {} | {}",
                    gl.get_parameter_string(glow::VENDOR),
                    gl.get_parameter_string(glow::RENDERER),
                    gl.get_parameter_string(glow::VERSION),
                ));
            }
            let (Some(render), Some(target)) = (&self.render, &self.target) else {
                return;
            };

            if std::mem::take(&mut self.clear_pending) {
                gl.disable(glow::SCISSOR_TEST);
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.fbo));
                gl.clear_color(0.0, 0.0, 0.0, 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
            }

            // `update` says whether mpv has a new frame; a resized target is
            // redrawn too, but that is not new video.
            let fresh = render.update();
            if fresh || self.needs_render {
                // mpv expects default GL state; egui_glow re-applies its own
                // state after this callback returns.
                gl.disable(glow::SCISSOR_TEST);
                gl.disable(glow::BLEND);
                gl.use_program(None);
                gl.bind_vertex_array(None);
                gl.bind_buffer(glow::ARRAY_BUFFER, None);
                gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
                match render.render(target.fbo.0.get(), width, height, glow::RGBA8 as i32) {
                    Ok(()) => {
                        self.needs_render = false;
                        if fresh {
                            self.frames_since_clear += 1;
                        }
                        if let Some(diag) = &mut self.diagnostics {
                            diag.renders += 1;
                        }
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }

            if let Some(diag) = &mut self.diagnostics {
                let due = diag.last_report.is_none_or(|t| t.elapsed() >= REPORT_EVERY);
                if due {
                    diag.last_report = Some(Instant::now());
                    // Sample the middle of the frame mpv drew: all-black for
                    // seconds while "Playing" means video is not getting through.
                    let mut px = [0u8; 4];
                    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(target.fbo));
                    gl.read_pixels(
                        width / 2,
                        height / 2,
                        1,
                        1,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(&mut px)),
                    );
                    let looks = if px[..3].iter().all(|&c| c < 10) {
                        "black"
                    } else {
                        "content"
                    };
                    diag.reports.push(format!(
                        "video: {} frames rendered, target {width}x{height}, center pixel {:?} ({looks})",
                        std::mem::take(&mut diag.renders),
                        &px[..3]
                    ));
                }
            }

            // Copy the frame into egui's framebuffer, clipped like the rest of egui.
            let clip = info.clip_rect_in_pixels();
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(target.fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, painter.intermediate_fbo());
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(
                clip.left_px,
                clip.from_bottom_px,
                clip.width_px,
                clip.height_px,
            );
            gl.blit_framebuffer(
                0,
                0,
                width,
                height,
                viewport.left_px,
                viewport.from_bottom_px,
                viewport.left_px + width,
                viewport.from_bottom_px + height,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, painter.intermediate_fbo());
        }
    }
}

/// Creates an RGBA8 texture of the given size attached to a new framebuffer.
unsafe fn create_target(gl: &glow::Context, width: i32, height: i32) -> Result<Target, String> {
    // SAFETY: the caller guarantees a current GL context.
    unsafe {
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            width,
            height,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.bind_texture(glow::TEXTURE_2D, None);

        let fbo = match gl.create_framebuffer() {
            Ok(fbo) => fbo,
            Err(e) => {
                gl.delete_texture(texture);
                return Err(e);
            }
        };
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        if status != glow::FRAMEBUFFER_COMPLETE {
            gl.delete_framebuffer(fbo);
            gl.delete_texture(texture);
            return Err(format!("video framebuffer incomplete (status {status:#x})"));
        }
        Ok(Target {
            fbo,
            texture,
            width,
            height,
        })
    }
}
