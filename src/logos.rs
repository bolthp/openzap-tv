//! Channel logos: fetched on background threads, cached on disk, decoded
//! and scaled down for display.
//!
//! Errors are never reported with the URL (it could carry a key); a logo
//! that cannot be loaded is simply not shown.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use crate::cache;
use crate::config::Platform;
use crate::source::{self, Source};

/// Logos larger than this are not downloaded.
pub const MAX_LOGO_BYTES: u64 = 2 * 1024 * 1024;
/// Logos are scaled down to fit this many pixels on each side.
pub const LOGO_SIZE: u32 = 96;
/// A cached logo is downloaded again after this long.
pub const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// Parallel downloads.
const WORKERS: usize = 4;
/// An idle worker thread exits after this long.
const IDLE_EXIT: Duration = Duration::from_secs(30);
/// Waiting downloads kept; older ones (scrolled past) are dropped and
/// asked for again if they come back into view.
const MAX_QUEUED: usize = 128;
/// Loaded logos not yet taken (their row was scrolled away); older ones
/// are dropped and loaded again (from the disk cache) when needed.
const MAX_UNCLAIMED: usize = 64;

/// Decoded RGBA pixels, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Answer to [`LogoLoader::take`].
#[derive(Debug, PartialEq, Eq)]
pub enum Logo {
    /// Queued or downloading; ask again on a later frame.
    Loading,
    /// Ready, handed over once: the caller keeps it (as a texture).
    Ready(Rgba),
    /// Cannot be loaded (asked again in this session, still `Failed`).
    Failed,
}

enum Slot {
    Queued,
    Ready(Rgba),
    Failed,
}

#[derive(Default)]
struct State {
    slots: HashMap<String, Slot>,
    /// Waiting URLs; the newest is served first (what is on screen now).
    queue: Vec<String>,
    /// Loaded but not yet taken, oldest first.
    unclaimed: VecDeque<String>,
    workers: usize,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    cache_dir: Option<PathBuf>,
    /// Called from a worker thread when a logo has finished loading.
    on_ready: Box<dyn Fn() + Send + Sync>,
}

impl State {
    /// Queues `url`, dropping the oldest waiting one past [`MAX_QUEUED`].
    fn enqueue(&mut self, url: &str) {
        self.slots.insert(url.to_string(), Slot::Queued);
        self.queue.push(url.to_string());
        if self.queue.len() > MAX_QUEUED {
            let oldest = self.queue.remove(0);
            self.slots.remove(&oldest);
        }
    }

    /// Stores a finished load; drops the oldest unclaimed logos past
    /// [`MAX_UNCLAIMED`].
    fn finish(&mut self, url: String, slot: Slot) {
        let ready = matches!(slot, Slot::Ready(_));
        self.slots.insert(url.clone(), slot);
        if !ready {
            return;
        }
        // Forget the ones taken meanwhile.
        let slots = &self.slots;
        self.unclaimed
            .retain(|u| matches!(slots.get(u), Some(Slot::Ready(_))));
        self.unclaimed.push_back(url);
        while self.unclaimed.len() > MAX_UNCLAIMED {
            if let Some(oldest) = self.unclaimed.pop_front() {
                self.slots.remove(&oldest);
            }
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Loads logos in the background. Cheap to call every frame.
pub struct LogoLoader {
    shared: Arc<Shared>,
}

impl LogoLoader {
    /// `cache_dir`: where downloaded logos are kept (`None`: no disk cache).
    /// `on_ready` runs on a worker thread after each load (e.g. to repaint).
    pub fn new(cache_dir: Option<PathBuf>, on_ready: impl Fn() + Send + Sync + 'static) -> Self {
        LogoLoader {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                wake: Condvar::new(),
                cache_dir,
                on_ready: Box::new(on_ready),
            }),
        }
    }

    /// The logo at `url` (or file path): starts loading it if needed.
    pub fn take(&self, url: &str) -> Logo {
        let mut state = self.shared.lock();
        match state.slots.get(url) {
            Some(Slot::Queued) => return Logo::Loading,
            Some(Slot::Failed) => return Logo::Failed,
            Some(Slot::Ready(_)) => {
                if let Some(Slot::Ready(rgba)) = state.slots.remove(url) {
                    return Logo::Ready(rgba);
                }
            }
            None => {}
        }
        state.enqueue(url);
        if state.workers < WORKERS {
            state.workers += 1;
            let shared = Arc::clone(&self.shared);
            std::thread::spawn(move || worker(&shared));
        }
        self.shared.wake.notify_one();
        Logo::Loading
    }

    /// Lets logos that failed be tried again (e.g. after a proxy change).
    pub fn forget_failed(&self) {
        self.shared
            .lock()
            .slots
            .retain(|_, slot| !matches!(slot, Slot::Failed));
    }
}

impl Drop for LogoLoader {
    fn drop(&mut self) {
        // Workers finish their current download and exit.
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
    }
}

fn worker(shared: &Shared) {
    loop {
        let url = {
            let mut state = shared.lock();
            loop {
                if state.shutdown {
                    state.workers -= 1;
                    return;
                }
                if let Some(url) = state.queue.pop() {
                    break url;
                }
                let (next, timeout) = shared
                    .wake
                    .wait_timeout(state, IDLE_EXIT)
                    .unwrap_or_else(|p| p.into_inner());
                state = next;
                if timeout.timed_out() && state.queue.is_empty() {
                    state.workers -= 1;
                    return;
                }
            }
        };
        let slot = match load(&url, shared.cache_dir.as_deref()) {
            Some(rgba) => Slot::Ready(rgba),
            None => Slot::Failed,
        };
        shared.lock().finish(url, slot);
        (shared.on_ready)();
    }
}

/// Reads (from cache or source) and decodes one logo.
fn load(url: &str, cache_dir: Option<&Path>) -> Option<Rgba> {
    let bytes = match Source::parse(url) {
        source @ Source::File(_) => source::fetch_bytes(&source, MAX_LOGO_BYTES).ok()?,
        source @ Source::Url(_) => {
            let cached = cache_dir.map(|dir| dir.join(cache_file_name(url)));
            fetch_cached(&source, cached.as_deref(), SystemTime::now())?
        }
    };
    decode(&bytes)
}

/// The cached copy if it is fresh; otherwise downloads (and caches) it.
/// If the download fails, a stale copy is still better than nothing.
fn fetch_cached(source: &Source, cached: Option<&Path>, now: SystemTime) -> Option<Vec<u8>> {
    let read_cache = || cached.and_then(|p| std::fs::read(p).ok());
    let fresh = cached
        .and_then(|p| std::fs::metadata(p).ok()?.modified().ok())
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age < CACHE_MAX_AGE);
    if fresh && let Some(bytes) = read_cache() {
        return Some(bytes);
    }
    match source::fetch_bytes(source, MAX_LOGO_BYTES) {
        Ok(bytes) => {
            if let Some(path) = cached {
                // A failed cache write only costs a download next time.
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(path, &bytes);
            }
            Some(bytes)
        }
        Err(_) => read_cache(),
    }
}

/// Decodes PNG or JPEG and scales it down to fit [`LOGO_SIZE`].
pub fn decode(bytes: &[u8]) -> Option<Rgba> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width() > LOGO_SIZE || image.height() > LOGO_SIZE {
        image.resize(LOGO_SIZE, LOGO_SIZE, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Some(Rgba {
        width: rgba.width(),
        height: rgba.height(),
        pixels: rgba.into_raw(),
    })
}

/// Stable file name for a URL (FNV-1a hash: the same in every build,
/// unlike std's `DefaultHasher`).
pub fn cache_file_name(url: &str) -> String {
    cache::file_name(url, "img")
}

/// Where logos are cached, given an environment lookup.
pub fn cache_dir(platform: Platform, env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    cache::root(platform, env).map(|root| root.join("logos"))
}

/// Logo cache location for the current platform and environment.
pub fn default_cache_dir() -> Option<PathBuf> {
    cache_dir(Platform::current(), |name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openzap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Asks until the loader is done with `url`.
    fn wait_for(loader: &LogoLoader, url: &str) -> Logo {
        for _ in 0..500 {
            match loader.take(url) {
                Logo::Loading => std::thread::sleep(Duration::from_millis(10)),
                done => return done,
            }
        }
        panic!("logo did not load");
    }

    #[test]
    fn decodes_and_scales_down_keeping_aspect() {
        let rgba = decode(&png(300, 150)).unwrap();
        assert_eq!((rgba.width, rgba.height), (LOGO_SIZE, LOGO_SIZE / 2));
        assert_eq!(rgba.pixels.len(), (rgba.width * rgba.height * 4) as usize);
        assert_eq!(&rgba.pixels[..4], &[200, 30, 30, 255]);

        let small = decode(&png(20, 10)).unwrap();
        assert_eq!((small.width, small.height), (20, 10));
        assert_eq!(decode(b"not an image"), None);
    }

    #[test]
    fn loads_local_files_once_and_reports_failures() {
        let dir = temp_dir("logo-file");
        let path = dir.join("logo.png");
        std::fs::write(&path, png(10, 10)).unwrap();
        let loader = LogoLoader::new(None, || {});
        let path = path.to_str().unwrap();

        assert!(matches!(wait_for(&loader, path), Logo::Ready(_)));
        // Handed over once; asking again loads it again.
        assert_eq!(loader.take(path), Logo::Loading);

        let missing = dir.join("missing.png");
        let missing = missing.to_str().unwrap();
        assert_eq!(wait_for(&loader, missing), Logo::Failed);
        assert_eq!(loader.take(missing), Logo::Failed, "not tried again");
        // Once it exists, a retry finds it.
        std::fs::write(missing, png(10, 10)).unwrap();
        loader.forget_failed();
        assert!(matches!(wait_for(&loader, missing), Logo::Ready(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fresh_cache_is_used_without_network() {
        let dir = temp_dir("logo-cache");
        // Nothing listens there; only the cache can answer.
        let url = "http://127.0.0.1:9/fresh.png";
        std::fs::write(dir.join(cache_file_name(url)), png(8, 8)).unwrap();
        let loader = LogoLoader::new(Some(dir.clone()), || {});
        assert!(matches!(wait_for(&loader, url), Logo::Ready(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_cache_is_used_when_the_download_fails() {
        let dir = temp_dir("logo-stale");
        let path = dir.join("stale.img");
        std::fs::write(&path, b"old").unwrap();
        let source = Source::parse("http://127.0.0.1:9/stale.png");
        let later = SystemTime::now() + CACHE_MAX_AGE + Duration::from_secs(60);
        assert_eq!(
            fetch_cached(&source, Some(&path), later).as_deref(),
            Some(&b"old"[..])
        );
        assert_eq!(fetch_cached(&source, None, later), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ready_callback_runs() {
        let dir = temp_dir("logo-callback");
        let path = dir.join("logo.png");
        std::fs::write(&path, png(4, 4)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let loader = LogoLoader::new(None, move || {
            let _ = tx.lock().unwrap().send(());
        });
        loader.take(path.to_str().unwrap());
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn tiny() -> Slot {
        Slot::Ready(Rgba {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
        })
    }

    #[test]
    fn old_waiting_downloads_are_dropped() {
        let mut state = State::default();
        for i in 0..MAX_QUEUED + 5 {
            state.enqueue(&format!("u{i}"));
        }
        assert_eq!(state.queue.len(), MAX_QUEUED);
        assert_eq!(state.slots.len(), MAX_QUEUED);
        // The first ones go; asked again, they are queued anew.
        assert!(!state.slots.contains_key("u0"));
        assert!(state.slots.contains_key(&format!("u{}", MAX_QUEUED + 4)));
        assert_eq!(state.queue.last().unwrap(), &format!("u{}", MAX_QUEUED + 4));
    }

    #[test]
    fn unclaimed_logos_are_limited() {
        let mut state = State::default();
        for i in 0..MAX_UNCLAIMED + 10 {
            state.finish(format!("u{i}"), tiny());
        }
        assert_eq!(state.slots.len(), MAX_UNCLAIMED);
        assert!(!state.slots.contains_key("u0"));
        assert!(state.slots.contains_key("u10"));
        // Taken ones no longer count; failures never do.
        state.slots.remove("u10");
        state.finish("bad".into(), Slot::Failed);
        state.finish("new".into(), tiny());
        assert!(state.slots.contains_key("u11"), "{:?}", state.unclaimed);
        assert!(state.slots.contains_key("bad"));
        assert_eq!(state.unclaimed.len(), MAX_UNCLAIMED);
    }

    #[test]
    fn cache_names_are_stable_and_distinct() {
        assert_eq!(cache_file_name(""), "cbf29ce484222325.img");
        assert_ne!(
            cache_file_name("http://example.com/a.png"),
            cache_file_name("http://example.com/b.png")
        );
    }

    #[test]
    fn logos_live_in_the_cache_folder() {
        let env = |name: &str| (name == "HOME").then(|| "/home/me".to_string());
        assert_eq!(
            cache_dir(Platform::Unix, env),
            Some(PathBuf::from("/home/me/.cache/openzap/logos"))
        );
    }
}
