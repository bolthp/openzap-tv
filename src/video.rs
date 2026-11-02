//! Embedded video: mpv renders into an offscreen texture, which is then
//! copied into the egui frame inside a paint callback.

use std::sync::{Arc, Mutex, MutexGuard};

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

#[derive(Default)]
struct State {
    render: Option<RenderContext>,
    target: Option<Target>,
    /// The target was (re)created and holds no frame yet.
    needs_render: bool,
    /// Last GL error from setting up the target, reported once.
    error: Option<String>,
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
            let (Some(render), Some(target)) = (&self.render, &self.target) else {
                return;
            };

            if render.update() || self.needs_render {
                // mpv expects default GL state; egui_glow re-applies its own
                // state after this callback returns.
                gl.disable(glow::SCISSOR_TEST);
                gl.disable(glow::BLEND);
                gl.use_program(None);
                gl.bind_vertex_array(None);
                gl.bind_buffer(glow::ARRAY_BUFFER, None);
                gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
                match render.render(target.fbo.0.get(), width, height) {
                    Ok(()) => self.needs_render = false,
                    Err(e) => self.error = Some(e.to_string()),
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
