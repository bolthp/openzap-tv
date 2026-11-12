//! The channel list panel: header, filters and the channel rows.

use std::sync::Arc;

use eframe::egui;
use openzap::browse::progress;
use openzap::clock;

use openzap::config::Side;

use super::logo::paint_logo;
use super::palette::palette;
use super::{Action, App, window_drag_background};

/// Width of the list when it slides in over the video.
const OVERLAY_WIDTH: f32 = 340.0;
/// Pointer this close to the window edge brings the list in, points.
const EDGE_PX: f32 = 6.0;
/// How long the list stays after the pointer leaves it, seconds.
pub(super) const LIST_LINGER_SECS: f64 = 1.5;
const SLIDE_SECS: f32 = 0.18;

/// The channel list over the video (fullscreen, or when it is not docked):
/// slides in at the window edge, slides away after the pointer leaves.
#[derive(Debug, Default)]
pub(super) struct ListOverlay {
    /// Stays open (the list button / `L` in fullscreen).
    pub(super) pinned: bool,
    until: f64,
    /// Where the list was drawn last frame.
    rect: Option<egui::Rect>,
}

impl ListOverlay {
    /// `at_edge`: pointer at the window edge; `over`: pointer over the list.
    pub(super) fn is_visible(&mut self, time: f64, at_edge: bool, over: bool) -> bool {
        if at_edge || over {
            self.until = time + LIST_LINGER_SECS;
        }
        self.pinned || time < self.until
    }

    /// The list is not drawn at all (docked or hidden).
    pub(super) fn hide(&mut self) {
        self.rect = None;
    }

    /// Where the list was drawn on the last frame, if it is out.
    pub(super) fn visible_rect(&self) -> Option<egui::Rect> {
        self.rect
    }

    fn remaining(&self, time: f64) -> Option<f64> {
        (time < self.until).then_some(self.until - time)
    }
}

/// Width : height of the logo box (logos are mostly landscape).
const LOGO_ASPECT: f32 = 1.5;

/// Colour of the favorite star.
const STAR: egui::Color32 = egui::Color32::from_rgb(240, 190, 40);

impl App {
    pub(super) fn channel_panel(&mut self, ui: &mut egui::Ui, now: i64) {
        window_drag_background(ui, self.fullscreen);
        self.list_header(ui);
        for notice in &self.notices {
            ui.colored_label(ui.visuals().error_fg_color, notice);
        }
        self.list_filters(ui);
        ui.add_space(4.0);
        self.list_rows(ui, now);
    }

    /// Draws the sliding list over the video within `screen`.
    pub(super) fn list_overlay(&mut self, ctx: &egui::Context, screen: egui::Rect, now: i64) {
        let time = ctx.input(|i| i.time);
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let left = self.config.view.list_side == Side::Left;
        let at_edge = pointer.is_some_and(|p| {
            screen.contains(p)
                && if left {
                    p.x <= screen.left() + EDGE_PX
                } else {
                    p.x >= screen.right() - EDGE_PX
                }
        });
        let over = match (pointer, self.list_overlay.rect) {
            (Some(p), Some(r)) => r.contains(p),
            _ => false,
        };
        // While the list is out, an open menu (group picker, context menu)
        // counts as using it; menus of the bottom bar must not pull it in.
        let menu_open = self.list_overlay.rect.is_some() && egui::Popup::is_any_open(ctx);
        let visible = self
            .list_overlay
            .is_visible(time, at_edge, over || menu_open);
        let shown =
            ctx.animate_bool_with_time(egui::Id::unique("list-overlay"), visible, SLIDE_SECS);
        if let Some(secs) = self.list_overlay.remaining(time) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(secs.max(0.05)));
        }
        self.list_on_screen(shown > 0.0);
        if shown <= 0.0 {
            self.list_overlay.rect = None;
            return;
        }
        let width = OVERLAY_WIDTH.min(screen.width() * 0.8);
        let x = if left {
            screen.left() - width * (1.0 - shown)
        } else {
            screen.right() - width * shown
        };
        let rect = egui::Rect::from_min_size(
            egui::pos2(x, screen.top()),
            egui::vec2(width, screen.height()),
        );
        self.list_overlay.rect = Some(rect);
        egui::Area::new(egui::Id::unique("list-overlay-area"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.set_min_size(rect.size() - egui::vec2(16.0, 16.0));
                        ui.set_max_size(rect.size() - egui::vec2(16.0, 16.0));
                        self.channel_panel(ui, now);
                    });
            });
    }

    fn list_header(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("OpenZap");
            // What is loading is told under the filters, where there is room.
            if let Some(text) = self.loading_text() {
                ui.spinner().on_hover_text(text);
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
        let count = format!("{} channels", self.visible.len());
        match self.loading_text() {
            Some(loading) => ui.weak(format!("{count} · {loading}")),
            None => ui.weak(count),
        };
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
        let mut play = None;
        let mut favorite = None;
        scroll.show_rows(ui, row_height, self.visible.len(), |ui, range| {
            for &idx in &self.visible[range] {
                let response = self.channel_row(ui, idx, row_height, now);
                if response.clicked() {
                    clicked = Some(idx);
                }
                if response.double_clicked() {
                    play = Some(idx);
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
            self.select(idx);
        }
        if let Some(idx) = play {
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
        let selected = self.selected == Some(idx);
        let playing = self.playing == Some(idx);
        let colors = palette(visuals);
        // (row fill, name, programme title, time span)
        let (fill, text, weak, time) = if selected {
            let c = visuals.selection.stroke.color;
            let dim = c.gamma_multiply(0.85);
            (visuals.selection.bg_fill, c, dim, dim)
        } else {
            let fill = if response.hovered() {
                visuals.widgets.hovered.weak_bg_fill
            } else {
                egui::Color32::TRANSPARENT
            };
            (fill, colors.primary, colors.secondary, colors.time)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, fill);
        if playing {
            // Accent stripe on the left marks the channel on screen.
            let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(4.0, rect.height()));
            let accent = if selected {
                visuals.selection.stroke.color
            } else {
                visuals.selection.bg_fill
            };
            painter.rect_filled(stripe, 2.0, accent);
        }
        let mut inner = rect.shrink2(egui::vec2(8.0, 5.0));
        // Channel number, right-aligned in a column wide enough for the
        // largest number in the playlist.
        let small = egui::TextStyle::Small.resolve(ui.style());
        let widest = self.numbers.iter().max().copied().unwrap_or(0).max(99);
        let column = painter
            .layout_no_wrap(widest.to_string(), small.clone(), weak)
            .size()
            .x;
        if let Some(n) = self.numbers.get(idx) {
            let number = painter.layout_no_wrap(n.to_string(), small.clone(), weak);
            let y = inner.center().y - number.size().y / 2.0;
            painter.galley(
                egui::pos2(inner.left() + column - number.size().x, y),
                number,
                weak,
            );
        }
        inner.min.x += column + 8.0;
        if self.show_logos {
            let logo = egui::Rect::from_min_size(
                inner.min,
                egui::vec2(inner.height() * LOGO_ASPECT, inner.height()),
            );
            let texture = self.logo_texture(idx);
            paint_logo(painter, logo, texture.as_ref(), &channel.name, text);
            inner.min.x = logo.right() + 8.0;
        }

        let body = egui::TextStyle::Body.resolve(ui.style());
        let mut x = inner.left();
        if self.is_favorite(idx) {
            let star = painter.layout_no_wrap("★ ".into(), body.clone(), STAR);
            let width = star.size().x;
            painter.galley(egui::pos2(x, inner.top()), star, STAR);
            x += width;
        }
        // Archive mark at the right end of the name line.
        let mut name_right = inner.right();
        if let Some(badge) = self.archive_badge(idx) {
            let badge = painter.layout_no_wrap(badge, small.clone(), time);
            let y = inner.top() + (body.size - small.size) / 2.0;
            name_right -= badge.size().x + 6.0;
            painter.galley(egui::pos2(inner.right() - badge.size().x, y), badge, time);
        }
        let name = elided(painter, &channel.name, body, text, name_right - x);
        painter.galley(egui::pos2(x, inner.top()), name, text);

        if let Some(p) = self.guide.now_next(&self.epg, idx, now).now {
            // Programme title on the left, its time span on the right.
            let span = format!("{}–{}", clock::local_hm(p.start), clock::local_hm(p.stop));
            let span = painter.layout_no_wrap(span, small.clone(), time);
            let title_width = inner.width() - span.size().x - 8.0;
            let title = elided(painter, &p.title, small, weak, title_width);
            let y = inner.bottom() - 4.0 - title.size().y;
            painter.galley(egui::pos2(inner.right() - span.size().x, y), span, time);
            painter.galley(egui::pos2(inner.left(), y), title, weak);
            let line_y = inner.bottom();
            let end = inner.left() + inner.width() * progress(p, now);
            painter.line_segment(
                [
                    egui::pos2(inner.left(), line_y),
                    egui::pos2(inner.right(), line_y),
                ],
                egui::Stroke::new(2.0, weak.gamma_multiply(0.2)),
            );
            painter.line_segment(
                [egui::pos2(inner.left(), line_y), egui::pos2(end, line_y)],
                egui::Stroke::new(2.0, time),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_opens_at_the_edge_and_lingers() {
        let mut list = ListOverlay::default();
        assert!(!list.is_visible(0.0, false, false));
        assert!(list.is_visible(1.0, true, false));
        // Moving onto the list keeps it open.
        assert!(list.is_visible(2.0, false, true));
        assert!(list.is_visible(2.0 + LIST_LINGER_SECS - 0.1, false, false));
        assert!(!list.is_visible(2.0 + LIST_LINGER_SECS, false, false));
    }

    #[test]
    fn pinned_overlay_stays() {
        let mut list = ListOverlay {
            pinned: true,
            ..ListOverlay::default()
        };
        assert!(list.is_visible(100.0, false, false));
    }
}
