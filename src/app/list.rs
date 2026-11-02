//! The channel list panel: header, filters and the channel rows.

use std::sync::Arc;

use eframe::egui;
use openzap::browse::progress;

use super::{Action, App};

/// Colour of the favorite star.
const STAR: egui::Color32 = egui::Color32::from_rgb(240, 190, 40);

impl App {
    pub(super) fn channel_panel(&mut self, ui: &mut egui::Ui, now: i64) {
        self.list_header(ui);
        for notice in &self.notices {
            ui.colored_label(ui.visuals().error_fg_color, notice);
        }
        self.list_filters(ui);
        ui.add_space(4.0);
        self.list_rows(ui, now);
    }

    fn list_header(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("OpenZap");
            if self.loading.is_some() {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("⚙").on_hover_text("Settings").clicked() {
                    self.actions.push(Action::OpenSettings);
                }
                let can_reload = self.loading.is_none() && !self.playlist_input.trim().is_empty();
                if ui
                    .add_enabled(can_reload, egui::Button::new("🔃"))
                    .on_hover_text("Load the playlist and guide again")
                    .clicked()
                {
                    self.actions.push(Action::Reload);
                }
                if ui
                    .button("⇄")
                    .on_hover_text("Move the list to the other side")
                    .clicked()
                {
                    self.actions.push(Action::SwapListSide);
                }
            });
        });
        ui.add_space(2.0);
    }

    fn list_filters(&mut self, ui: &mut egui::Ui) {
        let mut changed = ui
            .add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search channels")
                    .desired_width(f32::INFINITY),
            )
            .changed();
        ui.horizontal(|ui| {
            changed |= ui
                .toggle_value(&mut self.favorites_only, "★ Favorites")
                .on_hover_text("Show favorites only (right-click a channel to add it)")
                .changed();
            egui::ComboBox::from_id_salt("group")
                .selected_text(self.group.as_deref().unwrap_or("All groups"))
                .width(ui.available_width())
                .truncate()
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.group.is_none(), "All groups")
                        .clicked()
                    {
                        self.group = None;
                        changed = true;
                    }
                    for g in &self.groups {
                        let is_selected = self.group.as_deref() == Some(g);
                        if ui.selectable_label(is_selected, g).clicked() {
                            self.group = Some(g.clone());
                            changed = true;
                        }
                    }
                });
        });
        if changed {
            self.refilter();
        }
        ui.weak(format!("{} channels", self.visible.len()));
    }

    fn list_rows(&mut self, ui: &mut egui::Ui, now: i64) {
        let row_height = ui.text_style_height(&egui::TextStyle::Body)
            + ui.text_style_height(&egui::TextStyle::Small)
            + 14.0;
        let mut scroll = egui::ScrollArea::vertical().auto_shrink(false);
        // Keep the channel switched to with the keyboard in view.
        if let Some(target) = self.scroll_to.take()
            && let Some(pos) = self.visible.iter().position(|&i| i == target)
        {
            let step = row_height + ui.spacing().item_spacing.y;
            let offset = pos as f32 * step - (ui.available_height() - row_height) / 2.0;
            scroll = scroll.vertical_scroll_offset(offset.max(0.0));
        }

        let mut clicked = None;
        let mut favorite = None;
        scroll.show_rows(ui, row_height, self.visible.len(), |ui, range| {
            for &idx in &self.visible[range] {
                let response = self.channel_row(ui, idx, row_height, now);
                if response.clicked() {
                    clicked = Some(idx);
                }
                let is_favorite = self.is_favorite(idx);
                response.context_menu(|ui| {
                    let label = if is_favorite {
                        "Remove from favorites"
                    } else {
                        "Add to favorites"
                    };
                    if ui.button(label).clicked() {
                        favorite = Some(idx);
                        ui.close();
                    }
                });
            }
        });
        if let Some(idx) = clicked {
            self.selected = Some(idx);
            self.pending_play = Some(idx);
        }
        if let Some(idx) = favorite {
            self.actions.push(Action::ToggleFavorite(idx));
        }
    }

    /// One channel: name (with a star for favorites) and, below it, the
    /// programme on air with a thin progress line.
    fn channel_row(&self, ui: &mut egui::Ui, idx: usize, height: f32, now: i64) -> egui::Response {
        let size = egui::vec2(ui.available_width(), height);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let channel = &self.playlist.channels[idx];
        let visuals = ui.visuals();
        let active = match self.playing {
            Some(p) => p == idx,
            None => self.selected == Some(idx),
        };
        let (fill, text, weak) = if active {
            let c = visuals.selection.stroke.color;
            (visuals.selection.bg_fill, c, c.gamma_multiply(0.75))
        } else if response.hovered() {
            let w = &visuals.widgets.hovered;
            (
                w.weak_bg_fill,
                visuals.text_color(),
                visuals.weak_text_color(),
            )
        } else {
            (
                egui::Color32::TRANSPARENT,
                visuals.text_color(),
                visuals.weak_text_color(),
            )
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, fill);
        let inner = rect.shrink2(egui::vec2(8.0, 5.0));

        let body = egui::TextStyle::Body.resolve(ui.style());
        let small = egui::TextStyle::Small.resolve(ui.style());
        let mut x = inner.left();
        if self.is_favorite(idx) {
            let star = painter.layout_no_wrap("★ ".into(), body.clone(), STAR);
            let width = star.size().x;
            painter.galley(egui::pos2(x, inner.top()), star, STAR);
            x += width;
        }
        let name = elided(painter, &channel.name, body, text, inner.right() - x);
        painter.galley(egui::pos2(x, inner.top()), name, text);

        if let Some(p) = self.guide.now_next(&self.epg, idx, now).now {
            let title = elided(painter, &p.title, small, weak, inner.width());
            let y = inner.bottom() - 4.0 - title.size().y;
            painter.galley(egui::pos2(inner.left(), y), title, weak);
            let line_y = inner.bottom();
            let end = inner.left() + inner.width() * progress(p, now);
            painter.line_segment(
                [
                    egui::pos2(inner.left(), line_y),
                    egui::pos2(inner.right(), line_y),
                ],
                egui::Stroke::new(2.0, weak.gamma_multiply(0.25)),
            );
            painter.line_segment(
                [egui::pos2(inner.left(), line_y), egui::pos2(end, line_y)],
                egui::Stroke::new(2.0, weak),
            );
        }
        response
    }
}

/// Single-line text cut with "…" to fit `width`.
fn elided(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    width: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.0));
    painter.layout_job(job)
}
