//! The settings dialog. It edits a draft; nothing changes until Save.

use eframe::egui;
use openzap::config::{Config, Theme};
use openzap::player::{HardwareDecoding, PlaybackSettings};

/// Everything the dialog can change, copied from the current state.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsDraft {
    pub playlist: String,
    pub epg: String,
    pub resume_last: bool,
    pub playback: PlaybackSettings,
    pub theme: Theme,
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
                ui.add(
                    egui::TextEdit::singleline(&mut draft.playlist)
                        .hint_text("file path or URL (.m3u, .m3u8)")
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
                ui.label("Programme guide");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.epg)
                        .hint_text("optional: file path or URL (.xml, .xml.gz)")
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            });

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

        section(ui, "Startup");
        ui.checkbox(&mut draft.resume_last, "Resume the last channel on start");

        ui.add_space(12.0);
        ui.separator();
        let mut outcome = None;
        ui.horizontal(|ui| {
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
