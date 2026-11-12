//! The programme guide of one channel: every programme the EPG has for it,
//! grouped by day, sliding in over the video opposite the channel list.

use eframe::egui;
use openzap::browse::progress;
use openzap::clock;
use openzap::config::Side;
use openzap::epg::Programme;

use super::palette::palette;
use super::{Action, App, window_drag_background};
use openzap::config::MAX_SHIFT_MIN;

const WIDTH: f32 = 400.0;
const SLIDE_SECS: f32 = 0.18;

#[derive(Debug, Default)]
pub(super) struct ScheduleView {
    pub(super) open: bool,
    /// Channel shown last frame; a different one scrolls to "now" again.
    channel: Option<usize>,
    /// Start time of the programme whose description is shown.
    expanded: Option<i64>,
    scroll_to_now: bool,
    /// Where the panel was drawn last frame, if it is out.
    rect: Option<egui::Rect>,
    /// Time shift being dragged (minutes), applied when the drag ends.
    shift_draft: Option<i64>,
}

impl ScheduleView {
    pub(super) fn toggle(&mut self) {
        self.open = !self.open;
        self.scroll_to_now |= self.open;
    }

    /// Closes the panel; false if it was not open.
    pub(super) fn close(&mut self) -> bool {
        std::mem::replace(&mut self.open, false)
    }

    pub(super) fn visible_rect(&self) -> Option<egui::Rect> {
        self.rect
    }

    /// Tracks the channel shown; a new channel starts at "now", collapsed.
    fn follow(&mut self, channel: Option<usize>) {
        if self.channel != channel {
            self.channel = channel;
            self.expanded = None;
            self.scroll_to_now = true;
            self.shift_draft = None;
        }
    }

    fn toggle_expanded(&mut self, start: i64) {
        self.expanded = if self.expanded == Some(start) {
            None
        } else {
            Some(start)
        };
    }
}

impl App {
    /// Draws the panel within `screen` while it is open or sliding.
    pub(super) fn schedule_panel(&mut self, ctx: &egui::Context, screen: egui::Rect, now: i64) {
        let shown = ctx.animate_bool_with_time(
            egui::Id::unique("schedule"),
            self.schedule.open,
            SLIDE_SECS,
        );
        if shown <= 0.0 {
            self.schedule.rect = None;
            return;
        }
        let channel = self.guide_channel();
        self.schedule.follow(channel);
        let width = WIDTH.min(screen.width() * 0.8);
        // Opposite the channel list, so both can be out at once.
        let x = match self.config.view.list_side {
            Side::Left => screen.right() - width * shown,
            Side::Right => screen.left() - width * (1.0 - shown),
        };
        let rect = egui::Rect::from_min_size(
            egui::pos2(x, screen.top()),
            egui::vec2(width, screen.height()),
        );
        self.schedule.rect = Some(rect);

        let expanded = self.schedule.expanded;
        let scroll_now = std::mem::take(&mut self.schedule.scroll_to_now);
        let mut close = false;
        let mut clicked = None;
        let mut watch = None;
        let mut new_shift = None;
        let mut shift_draft = self.schedule.shift_draft;
        egui::Area::new(egui::Id::unique("schedule-area"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        let size = rect.size() - egui::vec2(24.0, 24.0);
                        ui.set_min_size(size);
                        ui.set_max_size(size);
                        window_drag_background(ui, self.fullscreen);
                        let colors = palette(ui.visuals());
                        ui.horizontal(|ui| {
                            ui.heading("Programme guide");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("✕").on_hover_text("Close (G, Esc)").clicked() {
                                        close = true;
                                    }
                                },
                            );
                        });
                        let Some(idx) = channel else {
                            ui.colored_label(colors.secondary, "Choose a channel");
                            return;
                        };
                        if let Some(c) = self.playlist.channels.get(idx) {
                            ui.label(egui::RichText::new(&c.name).strong().color(colors.primary));
                        }
                        new_shift = shift_control(ui, self.shift_minutes(idx), &mut shift_draft);
                        ui.separator();
                        let list = self.guide.schedule(&self.epg, idx);
                        let archive = self.archive_of(idx);
                        let watching = self.archive_shown(idx);
                        if list.is_empty() {
                            ui.colored_label(
                                colors.secondary,
                                "No programme guide for this channel",
                            );
                            return;
                        }
                        egui::ScrollArea::vertical()
                            .auto_shrink(false)
                            .show(ui, |ui| {
                                let today = clock::local_day(now);
                                let mut last_day = None;
                                for p in list {
                                    let day = clock::local_day(p.start);
                                    if last_day != Some(day) {
                                        last_day = Some(day);
                                        ui.add_space(6.0);
                                        let label = clock::day_label(day, today);
                                        ui.label(
                                            egui::RichText::new(label).strong().color(colors.time),
                                        );
                                    }
                                    let row = Row {
                                        open: expanded == Some(p.start),
                                        in_archive: archive.is_some_and(|a| a.covers(p.start, now)),
                                        watching: watching
                                            .is_some_and(|at| p.start <= at && at < p.stop),
                                    };
                                    let (response, watch_clicked) = schedule_row(ui, p, now, row);
                                    if response.clicked() {
                                        clicked = Some(p.start);
                                    }
                                    if watch_clicked {
                                        watch = Some((idx, p.start));
                                    }
                                    if scroll_now && p.start <= now && now < p.stop {
                                        response.scroll_to_me(Some(egui::Align::Center));
                                    }
                                }
                            });
                    });
            });
        if close {
            self.schedule.open = false;
        }
        if let Some(start) = clicked {
            self.schedule.toggle_expanded(start);
        }
        if let Some((channel, start)) = watch {
            self.actions.push(Action::PlayArchive { channel, start });
        }
        self.schedule.shift_draft = shift_draft;
        if let (Some(minutes), Some(channel)) = (new_shift, channel) {
            self.actions.push(Action::SetShift { channel, minutes });
        }
    }
}

/// Step of the − and + buttons of the time shift, minutes.
const SHIFT_STEP_MIN: i64 = 5;

/// "Time shift: +60 min" for the channel's guide. Dragging changes a
/// draft; the new value is returned once the drag ends or a value is typed.
fn shift_control(ui: &mut egui::Ui, current: i64, draft: &mut Option<i64>) -> Option<i64> {
    let colors = palette(ui.visuals());
    let mut minutes = draft.unwrap_or(current);
    let mut done = None;
    ui.horizontal(|ui| {
        ui.colored_label(colors.secondary, "Time shift");
        if ui
            .small_button("−")
            .on_hover_text(format!("{SHIFT_STEP_MIN} min earlier"))
            .clicked()
        {
            done = Some(current - SHIFT_STEP_MIN);
        }
        let response = ui
            .add(
                egui::DragValue::new(&mut minutes)
                    .range(-MAX_SHIFT_MIN..=MAX_SHIFT_MIN)
                    .speed(0.5)
                    .suffix(" min")
                    .custom_formatter(|n, _| format!("{n:+.0}")),
            )
            .on_hover_text(
                "Moves this channel's programme times (and where the archive \
                 starts them). Use it when the guide is ahead of or behind \
                 the picture: − and + move 5 min, or drag, or double-click \
                 and type minutes.",
            );
        if ui
            .small_button("+")
            .on_hover_text(format!("{SHIFT_STEP_MIN} min later"))
            .clicked()
        {
            done = Some(current + SHIFT_STEP_MIN);
        }
        if response.changed() {
            *draft = Some(minutes);
        }
        let finished = response.drag_stopped() || (response.changed() && !response.dragged());
        if current != 0
            && ui
                .small_button("Reset")
                .on_hover_text("Programme times as the guide has them")
                .clicked()
        {
            *draft = None;
            done = Some(0);
        } else if finished && done.is_none() {
            *draft = None;
            done = Some(minutes).filter(|&m| m != current);
        }
    });
    done
}

/// How a programme row is shown.
#[derive(Debug, Clone, Copy, Default)]
struct Row {
    /// The description is shown.
    open: bool,
    /// It can be played from the archive.
    in_archive: bool,
    /// It is being played from the archive.
    watching: bool,
}

/// Text of the archive button: past programmes are watched, the running
/// one is started over.
fn watch_label(p: &Programme, now: i64) -> &'static str {
    if p.stop <= now {
        "▶ Watch"
    } else {
        "▶ Watch from the start"
    }
}

/// One programme: start time and title; the current one is highlighted
/// with its progress; a click shows the description (and, for programmes
/// in the archive, a button to watch them). Returns the row's response and
/// whether the watch button was clicked.
fn schedule_row(ui: &mut egui::Ui, p: &Programme, now: i64, row: Row) -> (egui::Response, bool) {
    let colors = palette(ui.visuals());
    let past = p.stop <= now;
    let current = p.start <= now && now < p.stop;
    // Archived programmes stay readable: they can still be watched.
    let fade = |c: egui::Color32| {
        if past && !row.in_archive {
            c.gamma_multiply(0.55)
        } else {
            c
        }
    };
    let fill = if row.watching {
        colors.time.gamma_multiply(0.25)
    } else if current {
        ui.visuals().selection.bg_fill.gamma_multiply(0.35)
    } else {
        egui::Color32::TRANSPARENT
    };
    let inner = egui::Frame::new()
        .fill(fill)
        .corner_radius(4.0)
        .inner_margin(egui::vec2(6.0, 4.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = ui.available_width();
            job.append(
                &clock::local_hm(p.start),
                0.0,
                egui::TextFormat::simple(
                    egui::TextStyle::Monospace.resolve(ui.style()),
                    fade(colors.time),
                ),
            );
            job.append(
                &p.title,
                10.0,
                egui::TextFormat::simple(
                    egui::TextStyle::Body.resolve(ui.style()),
                    fade(colors.primary),
                ),
            );
            if row.in_archive {
                job.append(
                    "  ▶",
                    0.0,
                    egui::TextFormat::simple(
                        egui::TextStyle::Small.resolve(ui.style()),
                        colors.time.gamma_multiply(0.8),
                    ),
                );
            }
            ui.label(job);
            if current {
                let (line, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 3.0),
                    egui::Sense::hover(),
                );
                let mut done = line;
                done.set_width(line.width() * progress(p, now));
                ui.painter()
                    .rect_filled(line, 1.5, colors.secondary.gamma_multiply(0.2));
                ui.painter().rect_filled(done, 1.5, colors.time);
            }
            if row.open {
                let end = format!("{}–{}", clock::local_hm(p.start), clock::local_hm(p.stop));
                let meta = match &p.category {
                    Some(c) => format!("{end} · {c}"),
                    None => end,
                };
                ui.colored_label(fade(colors.time), meta);
                match &p.desc {
                    Some(desc) => ui.colored_label(colors.secondary, desc),
                    None => ui.colored_label(colors.secondary, "No description"),
                };
            }
        });
    let id = ui.scope_id().with(("programme", p.start));
    let mut response = ui
        .interact(inner.response.rect, id, egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let mut watch = false;
    if row.in_archive && !row.open {
        response = response.on_hover_text("In the archive: click, then Watch");
    }
    // Outside the row's click area: that one is registered later and would
    // take the button's clicks.
    if row.open && row.in_archive {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            watch = ui
                .button(watch_label(p, now))
                .on_hover_text("From the provider's archive")
                .clicked();
        });
    }
    (response, watch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_scrolls_to_now_and_closing_reports_state() {
        let mut view = ScheduleView::default();
        view.toggle();
        assert!(view.open && view.scroll_to_now);
        assert!(view.close());
        assert!(!view.close(), "already closed");
    }

    #[test]
    fn another_channel_collapses_and_scrolls_again() {
        let mut view = ScheduleView::default();
        view.follow(Some(1));
        view.scroll_to_now = false;
        view.toggle_expanded(100);
        assert_eq!(view.expanded, Some(100));
        view.follow(Some(1)); // same channel: unchanged
        assert_eq!(view.expanded, Some(100));
        assert!(!view.scroll_to_now);
        view.follow(Some(2));
        assert_eq!(view.expanded, None);
        assert!(view.scroll_to_now);
    }

    #[test]
    fn watch_button_names_what_happens() {
        let p = Programme {
            channel: "a".into(),
            start: 100,
            stop: 200,
            title: "T".into(),
            desc: None,
            category: None,
        };
        assert_eq!(watch_label(&p, 150), "▶ Watch from the start");
        assert_eq!(watch_label(&p, 200), "▶ Watch");
    }

    #[test]
    fn clicking_the_open_programme_collapses_it() {
        let mut view = ScheduleView::default();
        view.toggle_expanded(5);
        view.toggle_expanded(6);
        assert_eq!(view.expanded, Some(6));
        view.toggle_expanded(6);
        assert_eq!(view.expanded, None);
    }
}
