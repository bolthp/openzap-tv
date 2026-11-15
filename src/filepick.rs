//! "Open file" for the playlist and guide fields: the system's dialog, or
//! a built-in browser where the system cannot show one (Steam Deck's Game
//! Mode).
//!
//! The system dialog runs on its own thread (except on macOS, which
//! requires the main thread), so the window keeps drawing and video keeps
//! playing while it is open. The result arrives through a channel.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use eframe::egui;
use openzap::files::{self, Entry};

/// Which settings field the chosen file goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickTarget {
    Playlist,
    Epg,
}

impl PickTarget {
    fn title(self) -> &'static str {
        match self {
            PickTarget::Playlist => "Open playlist",
            PickTarget::Epg => "Open programme guide",
        }
    }

    /// (filter name, extensions); "All files" is always added.
    fn filter(self) -> (&'static str, &'static [&'static str]) {
        match self {
            PickTarget::Playlist => ("Playlists", &["m3u", "m3u8", "txt"]),
            PickTarget::Epg => ("XMLTV guides", &["xml", "gz", "xmltv"]),
        }
    }
}

/// A dialog that is open; poll it with [`Pick::poll`] every frame.
pub struct Pick {
    pub target: PickTarget,
    source: Source,
}

enum Source {
    System(Receiver<Option<PathBuf>>),
    Builtin(Box<FileBrowser>),
}

/// Result of polling an open dialog.
#[derive(Debug, PartialEq, Eq)]
pub enum PickState {
    Open,
    /// The user chose a file.
    Chosen(PathBuf),
    /// Cancelled, or the dialog could not be shown.
    Cancelled,
}

impl Pick {
    /// The dialog's state; draws the built-in browser.
    pub fn poll(&mut self, ctx: &egui::Context) -> PickState {
        match &mut self.source {
            Source::System(rx) => match rx.try_recv() {
                Ok(Some(path)) => PickState::Chosen(path),
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => PickState::Cancelled,
                Err(mpsc::TryRecvError::Empty) => PickState::Open,
            },
            Source::Builtin(browser) => match browser.show(ctx) {
                None => PickState::Open,
                Some(Some(path)) => PickState::Chosen(path),
                Some(None) => PickState::Cancelled,
            },
        }
    }

    /// "system" or "built-in", for the debug log.
    pub fn kind(&self) -> &'static str {
        match self.source {
            Source::System(_) => "system",
            Source::Builtin(_) => "built-in",
        }
    }

    #[cfg(test)]
    pub fn from_receiver(target: PickTarget, rx: Receiver<Option<PathBuf>>) -> Self {
        Pick {
            target,
            source: Source::System(rx),
        }
    }
}

/// Opens a dialog, starting in the folder of `current` if that is a local
/// path: the built-in browser if `builtin`, else the system's (`ctx` is
/// woken up when the user is done with it).
pub fn open(target: PickTarget, current: &str, builtin: bool, ctx: &egui::Context) -> Pick {
    let start = start_dir(current);
    if builtin {
        let home = files::home_dir(|name| std::env::var(name).ok());
        return Pick {
            target,
            source: Source::Builtin(Box::new(FileBrowser::new(target, start, home))),
        };
    }
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    let run = move || {
        let (name, extensions) = target.filter();
        let mut dialog = rfd::FileDialog::new()
            .set_title(target.title())
            .add_filter(name, extensions)
            .add_filter("All files", &["*"]);
        if let Some(dir) = start {
            dialog = dialog.set_directory(dir);
        }
        let _ = tx.send(dialog.pick_file());
        ctx.request_repaint();
    };
    if cfg!(target_os = "macos") {
        run();
    } else {
        std::thread::spawn(run);
    }
    Pick {
        target,
        source: Source::System(rx),
    }
}

/// Preferred size of the built-in browser, in points.
const BROWSER_SIZE: egui::Vec2 = egui::vec2(560.0, 520.0);
/// Space kept free around it on small windows.
const BROWSER_MARGIN: f32 = 24.0;
/// Height of everything but the list: title, places, path, buttons, frame.
const BROWSER_CHROME_HEIGHT: f32 = 170.0;
/// Rows are tall enough to hit with a finger.
const ROW_HEIGHT: f32 = 36.0;

/// What the built-in browser should do after a key press or click.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Stay,
    Open(PathBuf),
    Done(Option<PathBuf>),
}

/// A file browser drawn by the app: folders and matching files of one
/// folder, big rows for touch, arrow keys and Enter for gamepads mapped to
/// keys.
struct FileBrowser {
    target: PickTarget,
    dir: PathBuf,
    /// The listing, or why the folder cannot be read.
    entries: Result<Vec<Entry>, String>,
    all_files: bool,
    selected: usize,
    places: Vec<(String, PathBuf)>,
    /// Scroll the selected row into view in the next frame.
    reveal: bool,
}

impl FileBrowser {
    fn new(target: PickTarget, start: Option<PathBuf>, home: Option<PathBuf>) -> Self {
        let dir = files::first_dir(start, home.as_deref());
        let mut browser = FileBrowser {
            target,
            dir: PathBuf::new(),
            entries: Ok(Vec::new()),
            all_files: false,
            selected: 0,
            places: files::places(home.as_deref()),
            reveal: false,
        };
        browser.open_dir(dir);
        browser
    }

    fn open_dir(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.reload();
        self.selected = 0;
        self.reveal = true;
    }

    fn reload(&mut self) {
        let (_, extensions) = self.target.filter();
        let filter: &[&str] = if self.all_files { &[] } else { extensions };
        self.entries = files::list_dir(&self.dir, filter).map_err(|e| e.to_string());
        let len = self.entries.as_ref().map_or(0, Vec::len);
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    fn rows(&self) -> &[Entry] {
        self.entries.as_deref().unwrap_or(&[])
    }

    /// A row was chosen: open a folder, or finish with a file.
    fn activate(&self, index: usize) -> Step {
        match self.rows().get(index) {
            Some(e) if e.is_dir => Step::Open(e.path.clone()),
            Some(e) => Step::Done(Some(e.path.clone())),
            None => Step::Stay,
        }
    }

    fn parent(&self) -> Step {
        match self.dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => Step::Open(p.to_path_buf()),
            _ => Step::Stay,
        }
    }

    /// Up/Down move, Enter opens, Backspace goes up, Esc cancels.
    fn on_keys(&mut self, input: &egui::InputState) -> Step {
        let len = self.rows().len();
        if input.key_pressed(egui::Key::ArrowDown) && self.selected + 1 < len {
            self.selected += 1;
            self.reveal = true;
        }
        if input.key_pressed(egui::Key::ArrowUp) && self.selected > 0 {
            self.selected -= 1;
            self.reveal = true;
        }
        if input.key_pressed(egui::Key::Enter) {
            return self.activate(self.selected);
        }
        if input.key_pressed(egui::Key::Backspace) {
            return self.parent();
        }
        if input.key_pressed(egui::Key::Escape) {
            return Step::Done(None);
        }
        Step::Stay
    }

    fn apply(&mut self, step: Step) -> Option<Option<PathBuf>> {
        match step {
            Step::Stay => None,
            Step::Open(dir) => {
                self.open_dir(dir);
                None
            }
            Step::Done(result) => Some(result),
        }
    }

    /// Draws the browser; `Some` once the user chose a file (`Some(path)`)
    /// or cancelled (`None`).
    fn show(&mut self, ctx: &egui::Context) -> Option<Option<PathBuf>> {
        // The keys are the browser's: a focused widget below it (the
        // Browse… button) would take Enter or the arrows as well.
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        let keys = ctx.input(|i| self.on_keys(i));
        if let Some(done) = self.apply(keys) {
            return Some(done);
        }
        let screen = ctx.content_rect().size();
        let width = BROWSER_SIZE.x.min(screen.x - 2.0 * BROWSER_MARGIN).max(0.0);
        let list_height = (BROWSER_SIZE.y.min(screen.y - 2.0 * BROWSER_MARGIN)
            - BROWSER_CHROME_HEIGHT)
            .max(ROW_HEIGHT * 2.0);
        let mut step = Step::Stay;
        let response = egui::Modal::new(egui::Id::unique("file-browser")).show(ctx, |ui| {
            ui.set_width(width);
            ui.heading(self.target.title());
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("⬆ Up")
                    .on_hover_text("Parent folder (Backspace)")
                    .clicked()
                {
                    step = self.parent();
                }
                for (name, path) in &self.places {
                    if ui.selectable_label(*path == self.dir, name).clicked() {
                        step = Step::Open(path.clone());
                    }
                }
            });
            ui.weak(self.dir.to_string_lossy());
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(list_height)
                .min_scrolled_height(list_height)
                .auto_shrink([false, false])
                .show(ui, |ui| match &self.entries {
                    Err(e) => {
                        ui.label(format!("Cannot open this folder: {e}"));
                    }
                    Ok(rows) if rows.is_empty() => {
                        ui.weak("No matching files here.");
                    }
                    Ok(rows) => {
                        for (i, entry) in rows.iter().enumerate() {
                            let icon = if entry.is_dir { "📁" } else { "📄" };
                            let row = ui.add(
                                egui::Button::selectable(
                                    i == self.selected,
                                    format!("{icon}  {}", entry.name),
                                )
                                .min_size(egui::vec2(ui.available_width(), ROW_HEIGHT)),
                            );
                            if i == self.selected && self.reveal {
                                row.scroll_to_me(None);
                            }
                            if row.clicked() {
                                step = self.activate(i);
                            }
                        }
                    }
                });
            self.reveal = false;
            ui.separator();
            ui.horizontal(|ui| {
                let (name, _) = self.target.filter();
                if ui
                    .checkbox(&mut self.all_files, "All files")
                    .on_hover_text(format!("Off: only {name}"))
                    .changed()
                {
                    self.reload();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        step = Step::Done(None);
                    }
                });
            });
        });
        if step == Step::Stay && response.should_close() {
            step = Step::Done(None);
        }
        self.apply(step)
    }
}

/// The folder to start in: the one holding `current`, if it is an
/// existing local path (URLs and empty fields give `None`).
pub fn start_dir(current: &str) -> Option<PathBuf> {
    let current = current.trim();
    let lower = current.to_ascii_lowercase();
    if current.is_empty() || lower.starts_with("http://") || lower.starts_with("https://") {
        return None;
    }
    let path = Path::new(current);
    let dir = if path.is_dir() { path } else { path.parent()? };
    dir.is_dir().then(|| dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_next_to_the_current_file() {
        let dir = std::env::temp_dir().join(format!("openzap-pick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("list.m3u");
        std::fs::write(&file, "#EXTM3U\n").unwrap();

        assert_eq!(start_dir(file.to_str().unwrap()), Some(dir.clone()));
        assert_eq!(start_dir(dir.to_str().unwrap()), Some(dir.clone()));
        // A file that does not exist yet still opens its folder.
        assert_eq!(
            start_dir(dir.join("new.m3u").to_str().unwrap()),
            Some(dir.clone())
        );
        assert_eq!(start_dir("http://example.com/list.m3u"), None);
        assert_eq!(start_dir("  "), None);
        assert_eq!(start_dir("/no/such/dir/list.m3u"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn poll_reports_choice_cancel_and_waiting() {
        let ctx = egui::Context::default();
        let (tx, rx) = mpsc::channel();
        let mut pick = Pick::from_receiver(PickTarget::Playlist, rx);
        assert_eq!(pick.kind(), "system");
        assert_eq!(pick.poll(&ctx), PickState::Open);
        tx.send(Some(PathBuf::from("/tmp/a.m3u"))).unwrap();
        assert_eq!(
            pick.poll(&ctx),
            PickState::Chosen(PathBuf::from("/tmp/a.m3u"))
        );

        let (tx, rx) = mpsc::channel();
        let mut pick = Pick::from_receiver(PickTarget::Epg, rx);
        tx.send(None).unwrap();
        assert_eq!(pick.poll(&ctx), PickState::Cancelled);
        drop(tx);
        // A dialog thread that died counts as cancelled too.
        let (tx, rx) = mpsc::channel::<Option<PathBuf>>();
        drop(tx);
        assert_eq!(
            Pick::from_receiver(PickTarget::Epg, rx).poll(&ctx),
            PickState::Cancelled
        );
    }

    /// A folder with `lists/` and `a.m3u`, `b.m3u8`, `c.txt`, `d.xml`.
    fn sample_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("openzap-browse-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lists")).unwrap();
        for f in ["a.m3u", "b.m3u8", "c.txt", "d.xml"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        dir
    }

    fn names(browser: &FileBrowser) -> Vec<&str> {
        browser.rows().iter().map(|e| e.name.as_str()).collect()
    }

    /// One frame with `key` pressed, run through the browser's key handling.
    fn press(browser: &mut FileBrowser, key: egui::Key) -> Option<Option<PathBuf>> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut result = None;
        ctx.run_ui(input, |ui| {
            let step = ui.ctx().input(|i| browser.on_keys(i));
            result = browser.apply(step);
        })
        .textures_delta
        .clear();
        result
    }

    #[test]
    fn browser_lists_folders_and_matching_files() {
        let dir = sample_dir("list");
        let mut browser = FileBrowser::new(PickTarget::Playlist, Some(dir.clone()), None);
        assert_eq!(names(&browser), ["lists", "a.m3u", "b.m3u8", "c.txt"]);
        browser.all_files = true;
        browser.reload();
        assert_eq!(names(&browser).len(), 5);
        let guide = FileBrowser::new(PickTarget::Epg, Some(dir.clone()), None);
        assert_eq!(names(&guide), ["lists", "d.xml"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn browser_keys_move_open_go_up_and_choose() {
        let dir = sample_dir("keys");
        let mut browser = FileBrowser::new(PickTarget::Playlist, Some(dir.clone()), None);
        // Enter on the folder opens it (empty), Backspace comes back.
        assert_eq!(press(&mut browser, egui::Key::Enter), None);
        assert_eq!(browser.dir, dir.join("lists"));
        assert!(names(&browser).is_empty());
        assert_eq!(press(&mut browser, egui::Key::Backspace), None);
        assert_eq!(browser.dir, dir);
        // Down to the first file, Enter chooses it.
        press(&mut browser, egui::Key::ArrowDown);
        assert_eq!(browser.selected, 1);
        assert_eq!(
            press(&mut browser, egui::Key::Enter),
            Some(Some(dir.join("a.m3u")))
        );
        // Up stops at the top; Esc cancels.
        press(&mut browser, egui::Key::ArrowUp);
        press(&mut browser, egui::Key::ArrowUp);
        assert_eq!(browser.selected, 0);
        assert_eq!(press(&mut browser, egui::Key::Escape), Some(None));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn browser_survives_an_unreadable_folder() {
        let mut browser = FileBrowser::new(PickTarget::Playlist, None, None);
        browser.open_dir(PathBuf::from("/no/such/folder"));
        assert!(browser.entries.is_err());
        assert!(names(&browser).is_empty());
        assert_eq!(browser.activate(0), Step::Stay);
        assert_eq!(press(&mut browser, egui::Key::Enter), None);
    }
}
