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
    let response = egui::Modal::new(egui::Id::unique("settings")).show(ctx, |ui| {
        ui.set_width(520.0);
        ui.heading("Settings");
        ui.add_space(8.0);

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
    });
    response
        .inner
        .or_else(|| response.should_close().then_some(Outcome::Cancel))
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
    fn version_label_names_the_build() {
        assert_eq!(
            version_label(),
            format!("OpenZap {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
