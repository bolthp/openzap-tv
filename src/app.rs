//! egui application: channel browser with now/next details.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;
use openzap::browse::{filter_channels, format_duration, groups, progress};
use openzap::epg::{self, Epg, EpgChannel, Programme};
use openzap::guide::now_next_for;
use openzap::m3u::{self, Channel, Playlist};
use openzap::mask::mask_url;

pub struct App {
    playlist: Playlist,
    epg: Epg,
    groups: Vec<String>,
    /// Indices into `playlist.channels` that pass the current filter.
    visible: Vec<usize>,
    query: String,
    group: Option<String>,
    selected: Option<usize>,
    /// Load problems, shown in the UI.
    notices: Vec<String>,
}

impl App {
    /// `args`: optional playlist path, optional EPG path. No args -> synthetic demo data.
    pub fn from_args(args: &[String]) -> Self {
        let mut notices = Vec::new();
        let now = unix_now();
        let (playlist, epg) = match args.first() {
            None => demo_data(now),
            Some(path) => {
                let playlist = load(path, m3u::parse, &mut notices).unwrap_or_default();
                let epg = args
                    .get(1)
                    .and_then(|p| load(p, epg::parse, &mut notices))
                    .unwrap_or_default();
                (playlist, epg)
            }
        };
        Self::new(playlist, epg, notices)
    }

    fn new(playlist: Playlist, epg: Epg, notices: Vec<String>) -> Self {
        let groups = groups(&playlist);
        let visible = filter_channels(&playlist, "", None);
        Self {
            playlist,
            epg,
            groups,
            visible,
            query: String::new(),
            group: None,
            selected: None,
            notices,
        }
    }

    fn refilter(&mut self) {
        self.visible = filter_channels(&self.playlist, &self.query, self.group.as_deref());
    }

    fn channel_list(&mut self, ui: &mut egui::Ui) {
        ui.heading("OpenZap");
        let mut changed = ui.text_edit_singleline(&mut self.query).changed();

        egui::ComboBox::from_label("Group")
            .selected_text(self.group.as_deref().unwrap_or("All"))
            .show_ui(ui, |ui| {
                if ui.selectable_label(self.group.is_none(), "All").clicked() {
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
        if changed {
            self.refilter();
        }

        ui.label(format!("{} channels", self.visible.len()));
        ui.separator();

        let row_height = ui.text_style_height(&egui::TextStyle::Body);
        egui::ScrollArea::vertical().show_rows(ui, row_height, self.visible.len(), |ui, range| {
            for &idx in &self.visible[range] {
                let name = &self.playlist.channels[idx].name;
                if ui
                    .selectable_label(self.selected == Some(idx), name)
                    .clicked()
                {
                    self.selected = Some(idx);
                }
            }
        });
    }

    fn details(&self, ui: &mut egui::Ui, now: i64) {
        for notice in &self.notices {
            ui.colored_label(egui::Color32::LIGHT_RED, notice);
        }
        let Some(channel) = self.selected.and_then(|i| self.playlist.channels.get(i)) else {
            ui.label("Select a channel on the left.");
            return;
        };

        ui.heading(&channel.name);
        if let Some(group) = &channel.group_title {
            ui.label(format!("Group: {group}"));
        }
        // Never show the raw URL: it may contain an access key.
        ui.monospace(mask_url(&channel.url));
        ui.separator();

        let nn = now_next_for(channel, &self.epg, now);
        match nn.now {
            Some(p) => {
                ui.strong(format!("Now: {}", p.title));
                ui.add(
                    egui::ProgressBar::new(progress(p, now))
                        .text(format!("ends in {}", format_duration(p.stop - now))),
                );
                if let Some(desc) = &p.desc {
                    ui.label(desc);
                }
            }
            None => {
                ui.label("Now: no data");
            }
        }
        if let Some(p) = nn.next {
            ui.add_space(8.0);
            ui.label(format!(
                "Next: {} (in {})",
                p.title,
                format_duration(p.start - now)
            ));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = unix_now();
        egui::Panel::left("channels")
            .default_size(320.0)
            .show(ui, |ui| self.channel_list(ui));
        egui::CentralPanel::default().show(ui, |ui| self.details(ui, now));
        // Keep "ends in ..." fresh without burning CPU.
        ui.ctx().request_repaint_after(Duration::from_secs(15));
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Reads and parses a file; on failure records a notice and returns `None`.
fn load<T, E: std::fmt::Display>(
    path: &str,
    parse: impl FnOnce(&str) -> Result<T, E>,
    notices: &mut Vec<String>,
) -> Option<T> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            notices.push(format!("Cannot read {path}: {e}"));
            return None;
        }
    };
    match parse(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            notices.push(format!("Cannot parse {path}: {e}"));
            None
        }
    }
}

/// Synthetic data (example.com only) so the app is usable without any files.
pub fn demo_data(now: i64) -> (Playlist, Epg) {
    let entries = [
        ("Example News", "news.example", "News"),
        ("Example Film", "film.example", "Movies"),
        ("Example Sport", "sport.example", "Sports"),
    ];
    let mut playlist = Playlist::default();
    let mut epg = Epg::default();
    for (name, id, group) in entries {
        playlist.channels.push(Channel {
            name: name.into(),
            url: format!("http://example.com/{id}.m3u8?token=fakekey"),
            tvg_id: Some(id.into()),
            group_title: Some(group.into()),
            ..Channel::default()
        });
        epg.channels.push(EpgChannel {
            id: id.into(),
            display_name: name.into(),
            icon: None,
        });
        for (i, title) in ["Morning", "Midday", "Evening"].into_iter().enumerate() {
            let start = now - 1800 + i as i64 * 3600;
            epg.programmes.push(Programme {
                channel: id.into(),
                start,
                stop: start + 3600,
                title: format!("{name}: {title}"),
                desc: None,
                category: None,
            });
        }
    }
    (playlist, epg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_data_is_consistent() {
        let now = 1_000_000;
        let (playlist, epg) = demo_data(now);
        assert_eq!(playlist.channels.len(), 3);
        for ch in &playlist.channels {
            let nn = now_next_for(ch, &epg, now);
            assert!(nn.now.is_some(), "{} has no current programme", ch.name);
            assert!(nn.next.is_some(), "{} has no next programme", ch.name);
        }
    }

    #[test]
    fn missing_file_becomes_notice() {
        let app = App::from_args(&["/nonexistent/playlist.m3u".to_string()]);
        assert_eq!(app.notices.len(), 1);
        assert!(app.playlist.channels.is_empty());
    }
}
