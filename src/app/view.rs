//! The video area: picture, status overlays and the bottom bar with the
//! programme guide and playback controls, which slides in from below.

use std::time::Duration;

use eframe::egui;
use openzap::browse::{format_duration, progress};
use openzap::clock;
use openzap::config::Side;
use openzap::player::AspectMode;
use openzap::streaminfo::StreamInfo;

use super::logo::paint_logo;
use super::palette;
use super::{
    Action, App, InfoMode, StreamState, VOLUME_STEP, drag_window, draw_label, overlay_text,
};

/// How long the bar stays after the mouse stops moving, seconds.
pub(super) const BAR_IDLE_SECS: f64 = 3.0;
/// How long the bar is shown when a channel starts, seconds.
pub(super) const BAR_START_SECS: f64 = 5.0;
/// Slide animation length, seconds.
const BAR_ANIMATION_SECS: f32 = 0.18;

/// When the bottom bar is shown. Times are egui input times in seconds.
#[derive(Debug)]
pub(super) struct BarState {
    until: f64,
    /// Show for this long from the next frame on (set where no time is known).
    pending: Option<f64>,
    /// Height measured on the previous frame, used to place the bar.
    height: f32,
}

impl Default for BarState {
    fn default() -> Self {
        BarState {
            until: 0.0,
            pending: None,
            height: 150.0,
        }
    }
}

impl BarState {
    pub(super) fn show_for(&mut self, time: f64, secs: f64) {
        self.until = self.until.max(time + secs);
    }

    pub(super) fn request(&mut self, secs: f64) {
        self.pending = Some(self.pending.map_or(secs, |p| p.max(secs)));
    }

    /// `keep`: something holds the bar open (pointer over it, nothing playing).
    pub(super) fn is_visible(&mut self, time: f64, keep: bool) -> bool {
        if let Some(secs) = self.pending.take() {
            self.show_for(time, secs);
        }
        keep || time < self.until
    }

    /// Seconds until the bar hides by itself, if it is on a timer.
    fn remaining(&self, time: f64) -> Option<f64> {
        (time < self.until).then_some(self.until - time)
    }
}

impl App {
    /// The video with its overlays, filling the available space.
    pub(super) fn video_area(&mut self, ui: &mut egui::Ui, fullscreen: bool, now: i64) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        drag_window(&response, fullscreen);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, egui::Color32::BLACK);
        let time = ui.input(|i| i.time);

        let showing_video = self.playing.is_some() && self.video.is_attached();
        if showing_video {
            self.video.paint(ui, rect);
        }
        let hint = match (fullscreen, self.config.view.list_visible) {
            (false, true) => "Choose a channel in the list",
            (false, false) => "Nothing playing (L shows the channel list)",
            (true, _) => "Nothing playing",
        };
        let message =
            overlay_text(&self.stream).or_else(|| (!showing_video).then(|| hint.to_string()));
        if let Some(text) = message {
            draw_label(
                &painter,
                rect.center(),
                egui::Align2::CENTER_CENTER,
                &text,
                egui::TextStyle::Heading,
                ui,
            );
        }
        match self.info.shown(time) {
            InfoMode::Off => {}
            InfoMode::Programme => self.programme_info(ui, &painter, rect, now),
            InfoMode::Stream if showing_video => self.stream_info(ui, &painter, rect, time),
            InfoMode::Stream => {}
        }
        self.osd_message(ui, &painter, rect, time);
        if let Some(digits) = self.digits.text() {
            // The channel number being typed, large, where the OSD goes.
            let corner = rect.right_top() + egui::vec2(-16.0, 16.0);
            let font = egui::FontId::proportional(40.0);
            let galley = painter.layout_no_wrap(format!("{digits}_"), font, egui::Color32::WHITE);
            let text_rect = egui::Align2::RIGHT_TOP.anchor_size(corner, galley.size());
            painter.rect_filled(
                text_rect.expand(12.0),
                8.0,
                egui::Color32::from_black_alpha(190),
            );
            painter.galley(text_rect.min, galley, egui::Color32::WHITE);
        }

        // Mouse activity over the video brings the bar up.
        let pointer_over = ui.rect_contains_pointer(rect);
        let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
        if pointer_over && moved || response.clicked() {
            self.bar.show_for(time, BAR_IDLE_SECS);
        }
        if response.double_clicked() {
            self.actions.push(Action::ToggleFullscreen);
        }
        if response.hovered() {
            let scroll: f32 = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                        _ => None,
                    })
                    .sum()
            });
            if scroll != 0.0 {
                self.change_volume(VOLUME_STEP * f64::from(scroll.signum()), time);
            }
        }

        let bar_top = rect.bottom() - self.bar.height;
        let over_bar = pointer_over
            && ui
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|p| p.y >= bar_top);
        let idle = matches!(self.stream, StreamState::Idle | StreamState::Failed(_));
        let visible = self.bar.is_visible(time, over_bar || idle);
        let shown = ui.ctx().animate_bool_with_time(
            egui::Id::unique("bottom-bar"),
            visible,
            BAR_ANIMATION_SECS,
        );
        if shown > 0.0 {
            self.bottom_bar(ui, rect, shown, now, time);
        }
        self.lift_subtitles(self.bar.height * shown / rect.height().max(1.0));
        if let Some(secs) = self.bar.remaining(time) {
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(secs.max(0.05)));
        }
        if fullscreen && !visible && pointer_over {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        }
        if let Some(e) = self.video.take_error() {
            self.status = format!("Video error: {e}");
        }
    }

    /// The programme of the guide's channel in full, in the top-left
    /// corner: unlike the bar, with the whole description.
    fn programme_info(&self, ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect, now: i64) {
        let Some((idx, channel)) = self
            .guide_channel()
            .and_then(|i| self.playlist.channels.get(i).map(|c| (i, c)))
        else {
            return;
        };
        let colors = palette::palette(&palette::dark_visuals(ui.ctx()));
        let body = egui::TextStyle::Body.resolve(ui.style());
        let heading = egui::TextStyle::Heading.resolve(ui.style());
        let mut job = egui::text::LayoutJob::default();
        let mut add = |text: &str, font: &egui::FontId, color| {
            job.append(text, 0.0, egui::TextFormat::simple(font.clone(), color));
        };
        add(&channel.name, &heading, egui::Color32::WHITE);
        let nn = self.guide.now_next(&self.epg, idx, now);
        match nn.now {
            None => add(
                "\nNo programme guide for this channel",
                &body,
                colors.secondary,
            ),
            Some(p) => {
                let span = format!(
                    "\n{}–{}  ",
                    clock::local_hm(p.start),
                    clock::local_hm(p.stop)
                );
                add(&span, &body, colors.time);
                add(&p.title, &body, egui::Color32::WHITE);
                if let Some(category) = &p.category {
                    add(&format!("  · {category}"), &body, colors.secondary);
                }
                if let Some(desc) = &p.desc {
                    add(&format!("\n\n{desc}"), &body, colors.secondary);
                }
            }
        }
        if let Some(next) = nn.next {
            add(
                &format!("\n\nNext  {}  ", clock::local_hm(next.start)),
                &body,
                colors.time,
            );
            add(&next.title, &body, colors.secondary);
        }
        let margin = 16.0;
        job.wrap.max_width = (rect.width() * 0.5)
            .clamp(260.0, 600.0)
            .min(rect.width() - 2.0 * margin);
        // Long descriptions are cut to stay clear of the bar at the bottom.
        let room = rect.height() - self.bar.height - 3.0 * margin;
        job.wrap.max_rows = (room / ui.text_style_height(&egui::TextStyle::Body)).max(3.0) as usize;
        let galley = painter.layout_job(job);
        let pos = rect.left_top() + egui::vec2(margin, margin);
        let area = egui::Rect::from_min_size(pos, galley.size());
        painter.rect_filled(area.expand(10.0), 6.0, egui::Color32::from_black_alpha(190));
        painter.galley(pos, galley, egui::Color32::WHITE);
    }

    /// Technical details in the top-left corner (see `InfoOverlay`).
    fn stream_info(&mut self, ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect, time: f64) {
        if self.info.needs_refresh(time)
            && let Some(player) = &self.player
        {
            let info = StreamInfo::from_properties(|name| player.get_property(name));
            self.info.set_lines(info.lines(), time);
        }
        let mut text = self
            .playing
            .and_then(|i| self.playlist.channels.get(i))
            .map(|c| c.name.clone())
            .unwrap_or_default();
        for line in &self.info.lines {
            text.push('\n');
            text.push_str(line);
        }
        let corner = rect.left_top() + egui::vec2(16.0, 16.0);
        draw_label(
            painter,
            corner,
            egui::Align2::LEFT_TOP,
            &text,
            egui::TextStyle::Body,
            ui,
        );
    }

    /// Short messages (volume, mute, modes) in the top-right corner.
    fn osd_message(&mut self, ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect, time: f64) {
        let Some((text, until)) = &self.osd else {
            return;
        };
        if time < *until {
            let corner = rect.right_top() + egui::vec2(-16.0, 16.0);
            draw_label(
                painter,
                corner,
                egui::Align2::RIGHT_TOP,
                text,
                egui::TextStyle::Heading,
                ui,
            );
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(until - time));
        } else {
            self.osd = None;
        }
    }

    /// The bar at the bottom of the video; `shown` (0–1) is how far it has
    /// slid in.
    fn bottom_bar(
        &mut self,
        ui: &mut egui::Ui,
        video: egui::Rect,
        shown: f32,
        now: i64,
        time: f64,
    ) {
        let height = self.bar.height;
        let top = video.bottom() - height * shown;
        // Leave room for the channel list sliding over the video.
        let (mut left, mut right) = (video.left(), video.right());
        if let Some(list) = self.list_overlay.visible_rect() {
            match self.config.view.list_side {
                Side::Left => left = left.max(list.right()),
                Side::Right => right = right.min(list.left()),
            }
        }
        // The guide panel sits on the other side.
        if let Some(guide) = self.schedule.visible_rect() {
            match self.config.view.list_side {
                Side::Left => right = right.min(guide.left()),
                Side::Right => left = left.max(guide.right()),
            }
        }
        let bar = egui::Rect::from_min_max(
            egui::pos2(left, top),
            egui::pos2(right.max(left), top + height),
        );
        ui.painter().with_clip_rect(video).rect_filled(
            bar,
            0.0,
            egui::Color32::from_black_alpha(215),
        );

        let inner = bar.shrink2(egui::vec2(16.0, 12.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(video.intersect(ui.clip_rect()));
        // Always dark: the bar sits on top of the picture. (`visuals_mut`
        // changes only this Ui; `set_visuals` would change the whole app.)
        *child.visuals_mut() = palette::dark_visuals(ui.ctx());
        // Dragging over the guide text moves the window (see `video_area`).
        child.style_mut().interaction.selectable_labels = false;
        self.refresh_tracks(time);
        self.bar_guide(&mut child, now);
        child.add_space(8.0);
        self.bar_controls(&mut child);
        self.bar.height = child.min_rect().height() + 24.0;
    }

    /// Channel name and the programme guide for it.
    fn bar_guide(&self, ui: &mut egui::Ui, now: i64) {
        let colors = palette::palette(ui.visuals());
        let current = self.guide_channel();
        let Some((idx, channel)) =
            current.and_then(|i| self.playlist.channels.get(i).map(|c| (i, c)))
        else {
            ui.label(egui::RichText::new("No channel").heading());
            return;
        };
        ui.horizontal(|ui| {
            if self.show_logos {
                let (area, _) =
                    ui.allocate_exact_size(egui::vec2(54.0, 36.0), egui::Sense::hover());
                let texture = self.logo_texture(idx);
                paint_logo(
                    ui.painter(),
                    area,
                    texture.as_ref(),
                    &channel.name,
                    egui::Color32::WHITE,
                );
            }
            ui.label(egui::RichText::new(&channel.name).heading().strong());
            if let Some(group) = &channel.group_title {
                ui.colored_label(colors.secondary, group);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(clock::local_hm(now)).heading());
                // Failures are already shown in the middle of the video.
                let quiet = matches!(self.stream, StreamState::Playing | StreamState::Failed(_));
                if self.playing.is_some_and(|p| p != idx) {
                    // The guide shows a channel picked in the list, not the one on screen.
                    ui.colored_label(colors.secondary, "Selected · Enter to watch");
                } else if !quiet && !self.status.is_empty() {
                    ui.weak(&self.status);
                }
            });
        });

        if let Some(show) = self.archive_shown(idx) {
            ui.horizontal(|ui| {
                let day = clock::day_label(clock::local_day(show.start), clock::local_day(now));
                let span = format!(
                    "{day} {}–{}",
                    clock::local_hm(show.start),
                    clock::local_hm(show.stop)
                );
                ui.colored_label(colors.time, span);
                ui.strong(&show.title);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.colored_label(colors.secondary, "Archive");
                });
            });
            return;
        }
        let nn = self.guide.now_next(&self.epg, idx, now);
        // Channels without a guide just show their name.
        let Some(p) = nn.now else {
            return;
        };
        ui.horizontal(|ui| {
            let span = format!("{}–{}", clock::local_hm(p.start), clock::local_hm(p.stop));
            ui.colored_label(colors.time, span);
            ui.strong(&p.title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let left = format!("ends in {}", format_duration(p.stop - now));
                ui.colored_label(colors.secondary, left);
            });
        });
        let (line, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(line, 2.0, egui::Color32::from_white_alpha(40));
        let mut done = line;
        done.set_width(line.width() * progress(p, now));
        painter.rect_filled(done, 2.0, colors.time);

        if let Some(desc) = &p.desc {
            let mut job = egui::text::LayoutJob::single_section(
                desc.clone(),
                egui::TextFormat::simple(
                    egui::TextStyle::Body.resolve(ui.style()),
                    colors.secondary,
                ),
            );
            job.wrap = egui::text::TextWrapping {
                max_width: ui.available_width(),
                max_rows: 2,
                break_anywhere: false,
                overflow_character: Some('…'),
            };
            ui.label(job);
        }
        if let Some(next) = nn.next {
            ui.horizontal(|ui| {
                ui.colored_label(colors.time, clock::local_hm(next.start));
                ui.colored_label(colors.secondary, &next.title);
                let wait = format!("(in {})", format_duration(next.start - now));
                ui.colored_label(colors.secondary.gamma_multiply(0.8), wait);
            });
        }
    }

    fn bar_controls(&mut self, ui: &mut egui::Ui) {
        // Rough widths of the two groups; below that the view group wraps
        // onto its own row (e.g. when the guide panel narrows the bar).
        let audio = if self.tracks.len() > 1 { 200.0 } else { 0.0 }
            + if self.subtitles.is_empty() {
                0.0
            } else {
                170.0
            };
        let one_row = ui.available_width() >= 340.0 + 330.0 + audio;
        let right_to_left = egui::Layout::right_to_left(egui::Align::Center);
        if one_row {
            ui.horizontal(|ui| {
                self.playback_controls(ui);
                ui.with_layout(right_to_left, |ui| self.view_controls(ui));
            });
        } else {
            ui.horizontal(|ui| self.playback_controls(ui));
            ui.horizontal(|ui| ui.with_layout(right_to_left, |ui| self.view_controls(ui)));
        }
    }

    /// Stop/play, mute, volume, favorite.
    fn playback_controls(&mut self, ui: &mut egui::Ui) {
        let playing = self.playing.is_some();
        let (icon, hint) = if playing {
            ("⏹", "Stop")
        } else {
            ("▶", "Play")
        };
        let can_play = playing || self.selected.is_some();
        if ui
            .add_enabled(can_play, egui::Button::new(icon))
            .on_hover_text(hint)
            .clicked()
        {
            match (playing, self.selected) {
                (true, _) => self.actions.push(Action::Stop),
                (false, Some(idx)) => self.pending_play = Some(idx),
                (false, None) => {}
            }
        }
        if self.playing.or(self.current).is_some()
            && ui
                .button("↺")
                .on_hover_text("Restart the stream (R)")
                .clicked()
        {
            self.actions.push(Action::Restart);
        }
        if self.archive.is_some()
            && self.playing.is_some()
            && ui
                .button("Live")
                .on_hover_text("Back to the live stream")
                .clicked()
        {
            self.actions.push(Action::BackToLive);
        }
        let mute_icon = if self.muted { "🔇" } else { "🔊" };
        if ui.button(mute_icon).on_hover_text("Mute (M)").clicked() {
            self.actions.push(Action::ToggleMute);
        }
        let slider = ui.add(
            egui::Slider::new(&mut self.volume, 0.0..=100.0)
                .show_value(false)
                .trailing_fill(true),
        );
        if slider.changed() {
            self.pending_volume = true;
        }
        if slider.drag_stopped() {
            self.pending_save = true;
        }
        if let Some(idx) = self.guide_channel() {
            let (star, hint) = if self.is_favorite(idx) {
                ("★", "Remove from favorites")
            } else {
                ("☆", "Add to favorites")
            };
            if ui.button(star).on_hover_text(hint).clicked() {
                self.actions.push(Action::ToggleFavorite(idx));
            }
        }
    }

    /// Fullscreen, list, on top, guide, settings, picture size, audio
    /// (laid out right to left).
    fn view_controls(&mut self, ui: &mut egui::Ui) {
        if ui.button("⛶").on_hover_text("Fullscreen (F)").clicked() {
            self.actions.push(Action::ToggleFullscreen);
        }
        let list_shown = if self.fullscreen {
            self.list_overlay.pinned
        } else {
            self.config.view.list_visible
        };
        let list_hint = match (self.fullscreen, list_shown) {
            (true, false) => "Keep the channel list open (L)",
            (true, true) => "Let the channel list slide away (L)",
            (false, true) => "Hide the channel list (L)",
            (false, false) => "Show the channel list (L)",
        };
        if ui
            .selectable_label(list_shown, "☰")
            .on_hover_text(list_hint)
            .clicked()
        {
            self.actions.push(Action::ToggleList);
        }
        let pinned = self.on_top_supported && self.config.view.always_on_top;
        let pin = ui
            .add_enabled(
                self.on_top_supported,
                egui::Button::selectable(pinned, "📌"),
            )
            .on_hover_text("Always on top (T)")
            .on_disabled_hover_text(
                "On Wayland an app cannot keep its window on top. \
                 Use the window menu instead: Alt+Space (or right-click \
                 the title bar) → Always on Top.",
            );
        if pin.clicked() {
            self.actions.push(Action::ToggleOnTop);
        }
        if ui
            .selectable_label(self.schedule.open, "📅")
            .on_hover_text("Programme guide (G)")
            .clicked()
        {
            self.actions.push(Action::ToggleSchedule);
        }
        if ui.button("⚙").on_hover_text("Settings").clicked() {
            self.actions.push(Action::OpenSettings);
        }
        let aspect = self.current_aspect();
        egui::ComboBox::from_id_salt("aspect")
            .selected_text(aspect.label())
            .show_ui(ui, |ui| {
                for mode in AspectMode::ALL {
                    if ui.selectable_label(mode == aspect, mode.label()).clicked() {
                        self.actions.push(Action::SetAspect(mode));
                    }
                }
            })
            .response
            .on_hover_text("Picture size of this channel (Z)");
        if !self.subtitles.is_empty() {
            let current = self
                .subtitles
                .iter()
                .find(|t| t.selected)
                .map_or_else(|| "Subtitles off".to_string(), |t| t.label());
            egui::ComboBox::from_id_salt("subtitles")
                .selected_text(format!("CC {current}"))
                .width(150.0)
                .truncate()
                .show_ui(ui, |ui| {
                    let none = !self.subtitles.iter().any(|t| t.selected);
                    if ui.selectable_label(none, "Off").clicked() {
                        self.actions.push(Action::SetSubtitle(None));
                    }
                    for t in &self.subtitles {
                        if ui.selectable_label(t.selected, t.label()).clicked() {
                            self.actions.push(Action::SetSubtitle(Some(t.id)));
                        }
                    }
                })
                .response
                .on_hover_text("Subtitles (S)");
        }
        if self.tracks.len() > 1 {
            let current = self
                .tracks
                .iter()
                .find(|t| t.selected)
                .map_or_else(|| "Audio".to_string(), |t| t.label());
            egui::ComboBox::from_id_salt("audio")
                .selected_text(current)
                .width(180.0)
                .truncate()
                .show_ui(ui, |ui| {
                    for t in &self.tracks {
                        if ui.selectable_label(t.selected, t.label()).clicked() {
                            self.actions.push(Action::SetAudio(t.id));
                        }
                    }
                })
                .response
                .on_hover_text("Audio track (A)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_hides_after_its_time_unless_held() {
        let mut bar = BarState::default();
        assert!(!bar.is_visible(0.0, false));
        assert!(bar.is_visible(0.0, true));

        bar.show_for(10.0, BAR_IDLE_SECS);
        assert!(bar.is_visible(10.0 + BAR_IDLE_SECS - 0.1, false));
        assert!(!bar.is_visible(10.0 + BAR_IDLE_SECS, false));
        assert_eq!(bar.remaining(100.0), None);
    }

    #[test]
    fn requested_show_starts_on_next_frame_and_never_shortens() {
        let mut bar = BarState::default();
        bar.show_for(0.0, 10.0);
        bar.request(1.0); // shorter than what is already scheduled
        assert!(bar.is_visible(5.0, false));
        assert!(bar.is_visible(9.9, false));

        let mut bar = BarState::default();
        bar.request(BAR_START_SECS);
        assert!(bar.is_visible(50.0, false));
        assert!(bar.is_visible(50.0 + BAR_START_SECS - 0.1, false));
        assert!(!bar.is_visible(50.0 + BAR_START_SECS, false));
    }
}
