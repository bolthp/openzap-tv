//! The settings dialog. It edits a draft; nothing changes until Save.

use eframe::egui;
use openzap::cache;
use openzap::config::{Config, Theme, UpdatePolicy};
use openzap::player::{HardwareDecoding, PlaybackSettings};

use crate::filepick::PickTarget;

/// Everything the dialog can change, copied from the current state.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsDraft {
    pub playlist: String,
    pub epg: String,
    pub resume_last: bool,
    pub archive: bool,
    pub playback: PlaybackSettings,
    pub theme: Theme,
    pub playlist_update: UpdatePolicy,
    pub epg_update: UpdatePolicy,
    /// Size of the cache folder (`None`: no cache on disk).
    pub cache_size: Option<u64>,
    /// "Clear" was clicked; the app clears the cache right away.
    pub clear_cache: bool,
    /// A "browse" button was clicked; the app opens the file dialog.
    pub pick_request: Option<PickTarget>,
    /// A file dialog is open (the browse buttons are disabled meanwhile).
    pub picking: bool,
}

impl SettingsDraft {
    /// `playlist` and `epg` are the sources in use (they may come from the
    /// command line rather than the config).
    pub fn new(playlist: &str, epg: &str, config: &Config) -> Self {
        SettingsDraft {
            playlist: playlist.to_string(),
            epg: epg.to_string(),
            resume_last: config.resume_last,
            archive: config.archive,
            playback: config.playback,
            theme: config.view.theme,
            playlist_update: config.playlist_update,
            epg_update: config.epg_update,
            cache_size: None,
            clear_cache: false,
            pick_request: None,
            picking: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Save,
    Cancel,
}

/// Draws the dialog over the window; returns the user's decision once made.
/// Esc or a click outside counts as Cancel.
pub fn show(ctx: &egui::Context, draft: &mut SettingsDraft) -> Option<Outcome> {
    let response = show_modal(ctx, draft);
    response
        .inner
        .or_else(|| response.should_close().then_some(Outcome::Cancel))
}

/// Preferred width of the dialog, in points.
const DIALOG_WIDTH: f32 = 520.0;
/// Space kept free around the dialog on small windows.
const DIALOG_MARGIN: f32 = 24.0;
/// Height of everything but the scrolling part: title, button row and
/// the dialog's frame.
const DIALOG_CHROME_HEIGHT: f32 = 110.0;
/// The scrolling part never gets smaller than this.
const MIN_BODY_HEIGHT: f32 = 80.0;

/// Dialog width and the largest height of its scrolling part for a
/// window of the given size, so that the dialog (and its Save button)
/// always fits, e.g. on a 1280×800 Steam Deck.
fn dialog_size(window: egui::Vec2) -> (f32, f32) {
    let width = DIALOG_WIDTH.min(window.x - 2.0 * DIALOG_MARGIN).max(0.0);
    let body = (window.y - 2.0 * DIALOG_MARGIN - DIALOG_CHROME_HEIGHT).max(MIN_BODY_HEIGHT);
    (width, body)
}

fn show_modal(
    ctx: &egui::Context,
    draft: &mut SettingsDraft,
) -> egui::ModalResponse<Option<Outcome>> {
    let (width, body_height) = dialog_size(ctx.content_rect().size());
    egui::Modal::new(egui::Id::unique("settings")).show(ctx, |ui| {
        ui.set_width(width);
        ui.heading("Settings");
        ui.add_space(8.0);

        // The settings scroll when the window is too low for them; the
        // title and the buttons stay in view.
        egui::ScrollArea::vertical()
            .max_height(body_height)
            .show(ui, |ui| body(ui, draft));

        ui.add_space(12.0);
        ui.separator();
        let mut outcome = None;
        ui.horizontal(|ui| {
            ui.weak(version_label());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Save").clicked() {
                    outcome = Some(Outcome::Save);
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(Outcome::Cancel);
                }
            });
        });
        outcome
    })
}

/// The settings themselves (the scrolling part of the dialog).
fn body(ui: &mut egui::Ui, draft: &mut SettingsDraft) {
    section(ui, "Sources");
    egui::Grid::new("settings-sources")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Playlist");
            source_field(
                ui,
                &mut draft.playlist,
                "file path or URL (.m3u, .m3u8)",
                PickTarget::Playlist,
                &mut draft.pick_request,
                draft.picking,
            );
            ui.end_row();
            ui.label("Programme guide");
            source_field(
                ui,
                &mut draft.epg,
                "optional: file path or URL (.xml, .xml.gz)",
                PickTarget::Epg,
                &mut draft.pick_request,
                draft.picking,
            );
            ui.end_row();
        });

    section(ui, "Updates");
    egui::Grid::new("settings-updates")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Update the playlist");
            policy_combo(ui, "playlist-update", &mut draft.playlist_update);
            ui.end_row();
            ui.label("Update the guide");
            policy_combo(ui, "epg-update", &mut draft.epg_update);
            ui.end_row();
        });
    ui.small(
        "For sources from the internet: in between, the copy saved last time is used \
         (also when the server cannot be reached). Local files are read on every start. \
         The reload button in the channel list always downloads.",
    );

    section(ui, "Playback");
    egui::Grid::new("settings-playback")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Hardware decoding");
            egui::ComboBox::from_id_salt("hwdec")
                .selected_text(hwdec_label(draft.playback.hwdec))
                .show_ui(ui, |ui| {
                    for mode in [HardwareDecoding::Off, HardwareDecoding::Auto] {
                        ui.selectable_value(&mut draft.playback.hwdec, mode, hwdec_label(mode));
                    }
                });
            ui.end_row();
            ui.label("Network buffer");
            ui.add(
                egui::Slider::new(
                    &mut draft.playback.buffer_secs,
                    PlaybackSettings::MIN_BUFFER_SECS..=PlaybackSettings::MAX_BUFFER_SECS,
                )
                .suffix(" s"),
            );
            ui.end_row();
        });
    ui.small(
        "A larger buffer helps with unstable connections; switching channels may take longer.",
    );

    section(ui, "Archive");
    ui.checkbox(
        &mut draft.archive,
        "Show past programmes the provider keeps",
    );
    ui.small(
        "For playlists with an archive (catch-up). Recordings stay on the provider's \
         server; nothing is saved on this computer. Off: the guide keeps only a few \
         hours of the past, which uses less memory.",
    );

    section(ui, "Interface");
    egui::Grid::new("settings-interface")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Theme");
            egui::ComboBox::from_id_salt("theme")
                .selected_text(draft.theme.label())
                .show_ui(ui, |ui| {
                    for theme in Theme::ALL {
                        ui.selectable_value(&mut draft.theme, theme, theme.label());
                    }
                });
            ui.end_row();
        });

    if let Some(size) = draft.cache_size {
        section(ui, "Cache");
        ui.horizontal(|ui| {
            ui.label(format!(
                "Logos and saved playlists and guides: {}",
                cache::format_size(size)
            ));
            let clear = ui
                .add_enabled(size > 0, egui::Button::new("Clear"))
                .on_hover_text("Deletes them now; they are downloaded again when needed");
            if clear.clicked() {
                draft.clear_cache = true;
            }
        });
    }

    section(ui, "Startup");
    ui.checkbox(&mut draft.resume_last, "Resume the last channel on start");
}

/// A text field for a path or URL with a "browse" button after it.
fn source_field(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    target: PickTarget,
    request: &mut Option<PickTarget>,
    picking: bool,
) {
    ui.horizontal(|ui| {
        let button_width = 72.0;
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width(ui.available_width() - button_width - ui.spacing().item_spacing.x),
        );
        let browse = ui
            .add_enabled(
                !picking,
                egui::Button::new("Browse…").min_size(egui::vec2(button_width, 0.0)),
            )
            .on_hover_text("Choose a file");
        if browse.clicked() {
            *request = Some(target);
        }
    });
}

/// "OpenZap 0.4.1", shown at the bottom of the dialog.
fn version_label() -> &'static str {
    concat!("OpenZap ", env!("CARGO_PKG_VERSION"))
}

fn policy_combo(ui: &mut egui::Ui, id: &str, policy: &mut UpdatePolicy) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(policy.label())
        .width(220.0)
        .show_ui(ui, |ui| {
            for p in UpdatePolicy::ALL {
                ui.selectable_value(policy, p, p.label());
            }
        });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.strong(title);
    ui.add_space(2.0);
}

fn hwdec_label(mode: HardwareDecoding) -> &'static str {
    match mode {
        HardwareDecoding::Off => "Off (CPU)",
        HardwareDecoding::Auto => "Auto (GPU when possible)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_size_follows_the_window() {
        // Roomy window: preferred width, plenty of height.
        let (w, h) = dialog_size(egui::vec2(2560.0, 1440.0));
        assert_eq!(w, DIALOG_WIDTH);
        assert!(h > 1000.0);
        // Steam Deck: the whole dialog stays within 800 points.
        let (w, h) = dialog_size(egui::vec2(1280.0, 800.0));
        assert_eq!(w, DIALOG_WIDTH);
        assert!(h + DIALOG_CHROME_HEIGHT + 2.0 * DIALOG_MARGIN <= 800.0);
        // Narrow and low: shrinks, but never below the minimum.
        let (w, h) = dialog_size(egui::vec2(400.0, 100.0));
        assert_eq!(w, 400.0 - 2.0 * DIALOG_MARGIN);
        assert_eq!(h, MIN_BODY_HEIGHT);
        assert_eq!(dialog_size(egui::Vec2::ZERO).0, 0.0);
    }

    /// Lays the dialog out in a window of `size`; returns its rectangle.
    fn dialog_rect(size: egui::Vec2) -> egui::Rect {
        let ctx = egui::Context::default();
        let mut draft = SettingsDraft::new("", "", &Config::default());
        draft.cache_size = Some(1024); // the "Cache" section is shown too
        let mut rect = egui::Rect::NOTHING;
        // The first frames only measure; the layout settles after that.
        for _ in 0..4 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                rect = show_modal(ui.ctx(), &mut draft).response.rect;
            });
            output.textures_delta.clear(); // nothing renders them here
        }
        rect
    }

    #[test]
    fn dialog_fits_small_windows() {
        for size in [
            egui::vec2(1280.0, 800.0), // Steam Deck
            egui::vec2(1280.0, 720.0),
            egui::vec2(720.0, 480.0), // the smallest window allowed
        ] {
            let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let rect = dialog_rect(size);
            assert!(rect.is_positive(), "{size:?}: {rect:?}");
            assert!(window.contains_rect(rect), "{size:?}: {rect:?}");
        }
    }

    #[test]
    fn dialog_scrolls_only_when_the_window_is_low() {
        // With enough room the dialog is as tall as its contents.
        let tall = dialog_rect(egui::vec2(1920.0, 2000.0));
        let roomy = dialog_rect(egui::vec2(1920.0, 1200.0));
        assert_eq!(tall.height(), roomy.height());
        // The contents do not fit the smallest window: there they scroll.
        let low = dialog_rect(egui::vec2(720.0, 480.0));
        assert!(low.height() < tall.height());
    }

    #[test]
    fn version_label_names_the_build() {
        assert_eq!(
            version_label(),
            format!("OpenZap {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
