//! Native "Open file" dialog for the playlist and guide fields.
//!
//! The dialog runs on its own thread (except on macOS, which requires the
//! main thread), so the window keeps drawing and video keeps playing while
//! it is open. The result arrives through a channel.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use eframe::egui;

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

/// A dialog that is open; poll it with [`Pick::poll`].
pub struct Pick {
    pub target: PickTarget,
    rx: Receiver<Option<PathBuf>>,
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
    pub fn poll(&self) -> PickState {
        match self.rx.try_recv() {
            Ok(Some(path)) => PickState::Chosen(path),
            Ok(None) | Err(mpsc::TryRecvError::Disconnected) => PickState::Cancelled,
            Err(mpsc::TryRecvError::Empty) => PickState::Open,
        }
    }

    #[cfg(test)]
    pub fn from_receiver(target: PickTarget, rx: Receiver<Option<PathBuf>>) -> Self {
        Pick { target, rx }
    }
}

/// Opens the dialog, starting in the folder of `current` if that is a
/// local path; `ctx` is woken up when the user is done.
pub fn open(target: PickTarget, current: &str, ctx: &egui::Context) -> Pick {
    let (tx, rx) = mpsc::channel();
    let start = start_dir(current);
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
    Pick { target, rx }
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
        let (tx, rx) = mpsc::channel();
        let pick = Pick::from_receiver(PickTarget::Playlist, rx);
        assert_eq!(pick.poll(), PickState::Open);
        tx.send(Some(PathBuf::from("/tmp/a.m3u"))).unwrap();
        assert_eq!(pick.poll(), PickState::Chosen(PathBuf::from("/tmp/a.m3u")));

        let (tx, rx) = mpsc::channel();
        let pick = Pick::from_receiver(PickTarget::Epg, rx);
        tx.send(None).unwrap();
        assert_eq!(pick.poll(), PickState::Cancelled);
        drop(tx);
        // A dialog thread that died counts as cancelled too.
        let (tx, rx) = mpsc::channel::<Option<PathBuf>>();
        drop(tx);
        assert_eq!(
            Pick::from_receiver(PickTarget::Epg, rx).poll(),
            PickState::Cancelled
        );
    }
}
