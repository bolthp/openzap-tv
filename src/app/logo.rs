//! Channel logos as egui textures, on top of the library's `LogoLoader`.

use std::cell::RefCell;
use std::collections::HashMap;

use eframe::egui;
use openzap::logos::{self, Logo, LogoLoader};

/// Loaded logo textures by URL. `None` marks a logo that failed to load.
///
/// The map sits in a `RefCell` so drawing code that only has `&self`
/// (the list rows) can still add textures: the borrow is checked at run
/// time instead of compile time, which is fine on the single UI thread.
pub(super) struct Logos {
    loader: LogoLoader,
    textures: RefCell<HashMap<String, Option<egui::TextureHandle>>>,
}

impl Logos {
    pub(super) fn new(ctx: &egui::Context) -> Self {
        let repaint = ctx.clone();
        Logos {
            loader: LogoLoader::new(logos::default_cache_dir(), move || {
                repaint.request_repaint()
            }),
            textures: RefCell::new(HashMap::new()),
        }
    }

    /// The texture for `url` once it is loaded; starts loading otherwise.
    pub(super) fn texture(&self, ctx: &egui::Context, url: &str) -> Option<egui::TextureHandle> {
        if let Some(known) = self.textures.borrow().get(url) {
            return known.clone();
        }
        let texture = match self.loader.take(url) {
            Logo::Loading => return None,
            Logo::Failed => None,
            Logo::Ready(rgba) => {
                let size = [rgba.width as usize, rgba.height as usize];
                let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba.pixels);
                // Named by hash: texture names show up in debug tools, URLs must not.
                let name = format!("logo-{}", logos::cache_file_name(url));
                Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
            }
        };
        self.textures
            .borrow_mut()
            .insert(url.to_string(), texture.clone());
        texture
    }
}

/// Draws `texture` fitted into `area` (aspect kept, centred), or the first
/// letter of `name` as a placeholder.
pub(super) fn paint_logo(
    painter: &egui::Painter,
    area: egui::Rect,
    texture: Option<&egui::TextureHandle>,
    name: &str,
    color: egui::Color32,
) {
    match texture {
        Some(texture) => {
            let size = texture.size_vec2();
            let scale = (area.width() / size.x).min(area.height() / size.y);
            let rect = egui::Rect::from_center_size(area.center(), size * scale);
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
        }
        None => {
            painter.rect_filled(area, 4.0, color.gamma_multiply(0.12));
            let initial: String = name
                .chars()
                .find(|c| c.is_alphanumeric())
                .into_iter()
                .collect();
            painter.text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                initial.to_uppercase(),
                egui::FontId::proportional(area.height() * 0.5),
                color.gamma_multiply(0.6),
            );
        }
    }
}
