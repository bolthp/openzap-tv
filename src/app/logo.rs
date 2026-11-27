//! Channel logos as egui textures, on top of the library's `LogoLoader`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use eframe::egui;
use openzap::logos::{self, Logo, LogoLoader};

/// Logo textures kept at most (96×96 RGBA: about 18 MB). The ones not
/// drawn for longest go first and come back from the disk cache.
const MAX_TEXTURES: usize = 512;

/// Loaded logo textures by URL.
///
/// The maps sit in a `RefCell` so drawing code that only has `&self`
/// (the list rows) can still add textures: the borrow is checked at run
/// time instead of compile time, which is fine on the single UI thread.
pub(super) struct Logos {
    loader: LogoLoader,
    textures: RefCell<Recent<egui::TextureHandle>>,
    /// Logos that failed to load.
    failed: RefCell<HashSet<String>>,
}

impl Logos {
    pub(super) fn new(ctx: &egui::Context) -> Self {
        let repaint = ctx.clone();
        Logos {
            loader: LogoLoader::new(logos::default_cache_dir(), move || {
                repaint.request_repaint()
            }),
            textures: RefCell::new(Recent::new(MAX_TEXTURES)),
            failed: RefCell::new(HashSet::new()),
        }
    }

    /// The texture for `url` once it is loaded; starts loading otherwise.
    pub(super) fn texture(&self, ctx: &egui::Context, url: &str) -> Option<egui::TextureHandle> {
        if let Some(texture) = self.textures.borrow_mut().get(url) {
            return Some(texture.clone());
        }
        if self.failed.borrow().contains(url) {
            return None;
        }
        match self.loader.take(url) {
            Logo::Loading => None,
            Logo::Failed => {
                self.failed.borrow_mut().insert(url.to_string());
                None
            }
            Logo::Ready(rgba) => {
                let size = [rgba.width as usize, rgba.height as usize];
                let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba.pixels);
                // Named by hash: texture names show up in debug tools, URLs must not.
                let name = format!("logo-{}", logos::cache_file_name(url));
                let texture = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
                // A dropped handle frees the texture.
                self.textures
                    .borrow_mut()
                    .insert(url.to_string(), texture.clone());
                Some(texture)
            }
        }
    }

    /// Tries the logos that failed again (e.g. after a proxy change).
    pub(super) fn retry_failed(&self) {
        self.failed.borrow_mut().clear();
        self.loader.forget_failed();
    }
}

/// A map that keeps at most `cap` values, dropping the least recently
/// used one.
struct Recent<V> {
    /// Value and when it was last used.
    map: HashMap<String, (V, u64)>,
    clock: u64,
    cap: usize,
}

impl<V> Recent<V> {
    fn new(cap: usize) -> Self {
        Recent {
            map: HashMap::new(),
            clock: 0,
            cap,
        }
    }

    fn get(&mut self, key: &str) -> Option<&V> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(key).map(|(value, used)| {
            *used = clock;
            &*value
        })
    }

    fn insert(&mut self, key: String, value: V) {
        self.clock += 1;
        self.map.insert(key, (value, self.clock));
        if self.map.len() > self.cap {
            // A scan is fine for a few hundred entries, once per new logo.
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest {
                self.map.remove(&key);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_drops_the_least_recently_used() {
        let mut recent = Recent::new(2);
        recent.insert("a".into(), 1);
        recent.insert("b".into(), 2);
        assert_eq!(recent.get("a"), Some(&1)); // "b" is now the oldest
        recent.insert("c".into(), 3);
        assert_eq!(recent.get("b"), None);
        assert_eq!(recent.get("a"), Some(&1));
        assert_eq!(recent.get("c"), Some(&3));
        assert_eq!(recent.map.len(), 2);
    }
}
