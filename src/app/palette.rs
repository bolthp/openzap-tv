//! Colours beyond egui's defaults, for both themes.

use eframe::egui::{self, Color32};

/// Secondary text in the dark theme: egui's default "weak" grey is hard
/// to read on its dark grey panels.
const DARK_WEAK: Color32 = Color32::from_gray(165);

/// Colours for text that is not the main label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Palette {
    /// Main labels such as channel names.
    pub primary: Color32,
    /// Programme titles, descriptions, "next".
    pub secondary: Color32,
    /// Times and time spans.
    pub time: Color32,
}

pub(super) fn palette(visuals: &egui::Visuals) -> Palette {
    if visuals.dark_mode {
        Palette {
            primary: Color32::from_gray(235),
            secondary: Color32::from_gray(200),
            time: Color32::from_rgb(130, 190, 255),
        }
    } else {
        Palette {
            primary: Color32::from_gray(20),
            secondary: Color32::from_gray(60),
            time: Color32::from_rgb(20, 95, 170),
        }
    }
}

/// Adjusts egui's built-in dark style once at start-up.
pub(super) fn install(ctx: &egui::Context) {
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.visuals.weak_text_color = Some(DARK_WEAK);
    });
}

/// The dark visuals as installed (for the bar over the video).
pub(super) fn dark_visuals(ctx: &egui::Context) -> egui::Visuals {
    ctx.style_of(egui::Theme::Dark).visuals.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG-style relative luminance of an opaque colour.
    fn luminance(c: Color32) -> f32 {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (hi, lo) = {
            let (x, y) = (luminance(a), luminance(b));
            (x.max(y), x.min(y))
        };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn secondary_colours_are_readable_on_panels() {
        // 4.5:1 is the usual minimum for body text.
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let p = palette(&visuals);
            for colour in [p.primary, p.secondary, p.time] {
                let ratio = contrast(colour, visuals.panel_fill);
                assert!(
                    ratio >= 4.5,
                    "{colour:?} on {:?}: {ratio:.1}",
                    visuals.panel_fill
                );
            }
        }
        let dark = egui::Visuals::dark();
        assert!(contrast(DARK_WEAK, dark.panel_fill) >= 4.5);
    }
}
