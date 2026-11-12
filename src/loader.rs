//! Loads and parses the playlist and (optionally) the EPG from their
//! sources. Sources from the internet are saved as they download; the saved
//! copy is used instead when the update policy allows it, or when the
//! download fails.

use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use crate::cache;
use crate::catchup;
use crate::config::UpdatePolicy;
use crate::epg::{self, ChannelFilter, Epg, EpgError, EpgFilter, normalize_name};
use crate::m3u::{self, Playlist};
use crate::source::{self, Source};

/// Guides are streamed, so this only guards against endless or absurd input.
const MAX_EPG_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Programmes kept around "now": a little history for long shows already
/// running, and a few days ahead.
const EPG_KEEP_PAST_SECS: i64 = 3 * 3600;
const EPG_KEEP_FUTURE_SECS: i64 = 4 * 24 * 3600;
/// With the archive on, past programmes are kept as far back as the
/// archive goes, but not more than this (memory).
pub const ARCHIVE_MAX_DAYS: u32 = 14;
/// Downloaded data older than this is due for an update (except with
/// [`UpdatePolicy::Never`]).
pub const REFRESH_SECS: i64 = 24 * 3600;

/// How one source is handled by a load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// Not loaded: the app keeps the data it has.
    Keep,
    /// The saved copy if the policy allows it, else a download.
    Policy(UpdatePolicy),
    /// A download even if the saved copy is recent (the reload button).
    Download,
}

/// What to load. Errors and events never contain URLs.
#[derive(Debug, Clone)]
pub struct Request {
    pub playlist: String,
    pub epg: String,
    pub playlist_fetch: Fetch,
    pub epg_fetch: Fetch,
    /// Unix seconds: decides the age of saved copies and the guide window.
    pub now: i64,
    /// Where saved copies are kept (`None`: no saved copies).
    pub cache_dir: Option<PathBuf>,
    /// Guide channels to keep when the playlist is not (re)loaded; `None`
    /// keeps all.
    pub channels: Option<ChannelFilter>,
    /// `None`: the archive is off. Else the archive depth (days) of the
    /// playlist in use, for when the playlist is not (re)loaded.
    pub archive_days: Option<u32>,
}

/// Loaded data and where it came from.
#[derive(Debug)]
pub struct Fetched<T> {
    pub data: T,
    /// When it was downloaded (Unix seconds); `None` for local files.
    pub downloaded_at: Option<i64>,
    /// Read from the copy saved earlier, not downloaded now.
    pub from_saved: bool,
    /// The download failed, so the saved copy was used: why it failed.
    pub stale: Option<String>,
}

/// Sent as each source is done: the playlist first, so it can be shown
/// while a big guide is still loading.
#[derive(Debug)]
pub enum LoadEvent {
    Playlist(Result<Fetched<Playlist>, String>),
    /// `None`: no guide source given.
    Epg(Option<Result<Fetched<Epg>, String>>),
}

/// Blocking: call it from a background thread. Stops early (sending
/// nothing more) once `cancel` is set. Afterwards, saved copies of sources
/// no longer in use are deleted.
pub fn run(req: &Request, cancel: &AtomicBool, send: impl FnMut(LoadEvent)) {
    load(req, cancel, send);
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    if let Some(dir) = &req.cache_dir {
        let keep: Vec<String> = [(&req.playlist, "playlist"), (&req.epg, "epg")]
            .into_iter()
            .filter_map(|(input, kind)| saved_copy_name(input, kind))
            .collect();
        // Only costs disk space if it fails.
        let _ = cache::remove_unused(dir, &keep);
    }
}

/// File name of the saved copy of `input` (only sources from the internet
/// have one); `kind` is "playlist" or "epg".
fn saved_copy_name(input: &str, kind: &str) -> Option<String> {
    match Source::parse(input) {
        Source::Url(url) => Some(cache::file_name(&url, kind)),
        Source::File(_) => None,
    }
}

fn load(req: &Request, cancel: &AtomicBool, mut send: impl FnMut(LoadEvent)) {
    let cancelled = || cancel.load(Ordering::Relaxed);
    let mut channels = req.channels.clone();
    let mut archive_days = req.archive_days;
    if req.playlist_fetch != Fetch::Keep {
        let result = if req.playlist.trim().is_empty() {
            Err("no playlist source given".to_string())
        } else {
            let fetch = Fetching::new(req, &req.playlist, "playlist", req.playlist_fetch);
            fetch.load(source::MAX_TEXT_BYTES, cancel, parse_playlist)
        };
        if let Ok(fetched) = &result {
            channels = Some(channel_filter(&fetched.data));
            archive_days = archive_days.map(|_| catchup::max_days(&fetched.data));
        }
        if cancelled() {
            return;
        }
        send(LoadEvent::Playlist(result));
    }
    if req.epg_fetch == Fetch::Keep || cancelled() {
        return;
    }
    if req.epg.trim().is_empty() {
        send(LoadEvent::Epg(None));
        return;
    }
    let filter = EpgFilter {
        window: Some((
            req.now - keep_past_secs(archive_days),
            req.now + EPG_KEEP_FUTURE_SECS,
        )),
        channels,
    };
    let fetch = Fetching::new(req, &req.epg, "epg", req.epg_fetch);
    let result = fetch.load(MAX_EPG_BYTES, cancel, |reader, source, compressed| {
        epg::parse_reader(reader, &filter).map_err(|e| match e {
            EpgError::Io(kind) => source::read_error(source, kind, compressed).to_string(),
            other => other.to_string(),
        })
    });
    if !cancelled() {
        send(LoadEvent::Epg(Some(result)));
    }
}

/// How much guide history to keep: enough for the archive, if it is on.
fn keep_past_secs(archive_days: Option<u32>) -> i64 {
    let days = archive_days.unwrap_or(0).min(ARCHIVE_MAX_DAYS);
    EPG_KEEP_PAST_SECS.max(i64::from(days) * 24 * 3600)
}

/// Whether the saved copy (saved at `saved_at`) is used instead of a download.
pub fn use_saved_copy(fetch: Fetch, saved_at: Option<i64>, now: i64) -> bool {
    match (fetch, saved_at) {
        (Fetch::Keep | Fetch::Download, _) | (_, None) => false,
        (Fetch::Policy(UpdatePolicy::OnStart), _) => false,
        (Fetch::Policy(UpdatePolicy::Daily), Some(at)) => now - at < REFRESH_SECS,
        (Fetch::Policy(UpdatePolicy::Never), Some(_)) => true,
    }
}

/// Whether data downloaded at `downloaded_at` should be updated now, while
/// the app runs. Local files (`None`) are only read at start.
pub fn update_due(policy: UpdatePolicy, downloaded_at: Option<i64>, now: i64) -> bool {
    policy != UpdatePolicy::Never && downloaded_at.is_some_and(|at| now - at >= REFRESH_SECS)
}

/// Parses data read from `source` (`compressed`: it was gzipped).
type Parse<'a, T> = dyn Fn(&mut dyn BufRead, &Source, bool) -> Result<T, String> + 'a;

/// One source of a load.
struct Fetching {
    source: Source,
    fetch: Fetch,
    now: i64,
    /// The saved copy's path (sources from the internet only).
    saved: Option<PathBuf>,
}

impl Fetching {
    fn new(req: &Request, input: &str, kind: &str, fetch: Fetch) -> Self {
        let source = Source::parse(input);
        let saved = req
            .cache_dir
            .as_ref()
            .zip(saved_copy_name(input, kind))
            .map(|(dir, name)| dir.join(name));
        Fetching {
            source,
            fetch,
            now: req.now,
            saved,
        }
    }

    fn load<T>(
        &self,
        max_bytes: u64,
        cancel: &AtomicBool,
        parse: impl Fn(&mut dyn BufRead, &Source, bool) -> Result<T, String>,
    ) -> Result<Fetched<T>, String> {
        let parse: &Parse<T> = &parse;
        let Source::Url(_) = &self.source else {
            // A local file is read every time: it is its own copy.
            let data = read(&self.source, None, max_bytes, cancel, parse)?;
            return Ok(Fetched {
                data,
                downloaded_at: None,
                from_saved: false,
                stale: None,
            });
        };
        let saved = self.saved.as_deref();
        let saved_at = saved.and_then(modified_unix);
        let from_saved = || -> Option<T> {
            let path = saved?;
            read(
                &Source::File(path.to_path_buf()),
                None,
                max_bytes,
                cancel,
                parse,
            )
            .ok()
        };
        if use_saved_copy(self.fetch, saved_at, self.now)
            && let Some(data) = from_saved()
        {
            return Ok(Fetched {
                data,
                downloaded_at: saved_at,
                from_saved: true,
                stale: None,
            });
        }
        match read(&self.source, saved, max_bytes, cancel, parse) {
            Ok(data) => Ok(Fetched {
                data,
                downloaded_at: Some(self.now),
                from_saved: false,
                stale: None,
            }),
            Err(e) => match saved_at.filter(|_| !cancel.load(Ordering::Relaxed)) {
                Some(at) => from_saved().map_or(Err(e.clone()), |data| {
                    Ok(Fetched {
                        data,
                        downloaded_at: Some(at),
                        from_saved: true,
                        stale: Some(e),
                    })
                }),
                None => Err(e),
            },
        }
    }
}

/// Reads and parses `source`; with `save_to`, the raw bytes are saved there
/// too (replacing the old copy only once everything was read and parsed).
fn read<T>(
    source: &Source,
    save_to: Option<&Path>,
    max_bytes: u64,
    cancel: &AtomicBool,
    parse: &Parse<T>,
) -> Result<T, String> {
    let part = save_to.map(part_path);
    let copy = part.as_deref().and_then(|part| {
        std::fs::create_dir_all(part.parent()?).ok()?;
        Some(source::Copy {
            file: cache::create_private(part).ok()?,
            ok: Arc::new(AtomicBool::new(true)),
        })
    });
    let copy_ok = copy.as_ref().map(|c| c.ok.clone());
    let opened = source::open_copying(source, max_bytes, copy).map_err(|e| e.to_string());
    let result = opened.and_then(|opened| {
        let compressed = opened.compressed;
        let mut reader = Cancellable {
            inner: opened.reader,
            cancel,
        };
        let data = parse(&mut reader, source, compressed)?;
        // Read to the end, so the saved copy is complete.
        io::copy(&mut reader, &mut io::sink())
            .map_err(|e| source::read_error(source, e.kind(), compressed).to_string())?;
        Ok(data)
        // `reader` (and the copy's file) is closed here, before the rename.
    });
    if let (Some(part), Some(saved)) = (part, save_to) {
        let complete = result.is_ok() && copy_ok.is_some_and(|ok| ok.load(Ordering::Relaxed));
        if complete && !cancel.load(Ordering::Relaxed) {
            // A failed rename only costs the saved copy.
            let _ = std::fs::rename(&part, saved);
        } else if !cancel.load(Ordering::Relaxed) {
            // Left alone when cancelled: a newer load may be writing it.
            let _ = std::fs::remove_file(&part);
        }
    }
    result
}

fn parse_playlist(
    reader: &mut dyn BufRead,
    source: &Source,
    compressed: bool,
) -> Result<Playlist, String> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| source::read_error(source, e.kind(), compressed).to_string())?;
    m3u::parse(&String::from_utf8_lossy(&bytes)).map_err(|e| e.to_string())
}

/// The copy being written: `<saved>.part`.
fn part_path(saved: &Path) -> PathBuf {
    let mut name = saved.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// A file's modification time in Unix seconds.
fn modified_unix(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// Fails every read once the load is cancelled.
struct Cancellable<'a, R> {
    inner: R,
    cancel: &'a AtomicBool,
}

impl<R> Cancellable<'_, R> {
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(io::Error::other("load cancelled"))
        } else {
            Ok(())
        }
    }
}

impl<R: Read> Read for Cancellable<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        self.inner.read(buf)
    }
}

impl<R: BufRead> BufRead for Cancellable<'_, R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.check()?;
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

/// The EPG channels a playlist can match (same rules as `GuideIndex`).
pub fn channel_filter(playlist: &Playlist) -> ChannelFilter {
    let mut filter = ChannelFilter::default();
    for c in &playlist.channels {
        if let Some(id) = &c.tvg_id {
            filter.ids.insert(normalize_name(id));
            // Also the id without an iptv-org feed ("x.es@SD" -> "x.es").
            if let Some((base, _)) = id.split_once('@') {
                filter.ids.insert(normalize_name(base));
            }
        }
        for name in [c.tvg_name.as_deref(), Some(c.name.as_str())]
            .into_iter()
            .flatten()
        {
            filter.names.insert(normalize_name(name));
        }
    }
    filter
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("openzap-loader-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_file(name: &str, data: &str) -> String {
        let dir = std::env::temp_dir().join(format!("openzap-loader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// Serves `body` to `times` requests, then stops listening.
    fn serve(body: &'static str, times: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..times {
                let (mut stream, _) = listener.accept().unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(header.as_bytes()).unwrap();
                stream.write_all(body.as_bytes()).unwrap();
            }
        });
        format!("http://{addr}/list.m3u?token=fakekey")
    }

    /// A URL nobody listens on (connection refused at once).
    const DEAD_URL: &str = "http://127.0.0.1:1/list.m3u?token=fakekey";

    const PLAYLIST: &str = "#EXTM3U\n#EXTINF:-1 tvg-id=\"a\",A\nhttp://example.com/a\n";
    const EPG: &str = "<tv><channel id=\"a\"><display-name>A</display-name></channel></tv>";

    fn request(playlist: &str, epg: &str) -> Request {
        Request {
            playlist: playlist.into(),
            epg: epg.into(),
            playlist_fetch: Fetch::Policy(UpdatePolicy::OnStart),
            epg_fetch: Fetch::Policy(UpdatePolicy::OnStart),
            now: 0,
            cache_dir: None,
            channels: None,
            archive_days: None,
        }
    }

    fn events(req: &Request) -> Vec<LoadEvent> {
        let mut out = Vec::new();
        run(req, &AtomicBool::new(false), |e| out.push(e));
        out
    }

    fn playlist_of(events: &[LoadEvent]) -> &Result<Fetched<Playlist>, String> {
        events
            .iter()
            .find_map(|e| match e {
                LoadEvent::Playlist(p) => Some(p),
                LoadEvent::Epg(_) => None,
            })
            .expect("no playlist event")
    }

    fn epg_of(events: &[LoadEvent]) -> &Option<Result<Fetched<Epg>, String>> {
        events
            .iter()
            .find_map(|e| match e {
                LoadEvent::Epg(e) => Some(e),
                LoadEvent::Playlist(_) => None,
            })
            .expect("no epg event")
    }

    #[test]
    fn playlist_comes_first_then_the_guide() {
        let ev = events(&request(
            &temp_file("b.m3u", PLAYLIST),
            &temp_file("b.xml", EPG),
        ));
        assert_eq!(ev.len(), 2);
        assert!(matches!(ev[0], LoadEvent::Playlist(Ok(_))));
        let epg = epg_of(&ev).as_ref().unwrap().as_ref().unwrap();
        assert_eq!(epg.data.channels.len(), 1);
        assert_eq!(epg.downloaded_at, None, "local file");
    }

    #[test]
    fn no_guide_source_is_reported_as_none() {
        let ev = events(&request(&temp_file("a.m3u", PLAYLIST), "  "));
        assert_eq!(playlist_of(&ev).as_ref().unwrap().data.channels.len(), 1);
        assert!(epg_of(&ev).is_none());
    }

    #[test]
    fn kept_sources_are_not_loaded() {
        let mut req = request(DEAD_URL, &temp_file("k.xml", EPG));
        req.playlist_fetch = Fetch::Keep;
        req.channels = Some(ChannelFilter::default()); // the app's playlist matches nothing
        let ev = events(&req);
        assert_eq!(ev.len(), 1, "only the guide");
        let epg = epg_of(&ev).as_ref().unwrap().as_ref().unwrap();
        assert!(
            epg.data.channels.is_empty(),
            "filtered by the given channels"
        );

        req.epg_fetch = Fetch::Keep;
        assert!(events(&req).is_empty());
    }

    #[test]
    fn cancelled_load_sends_nothing() {
        let req = request(&temp_file("c.m3u", PLAYLIST), &temp_file("c.xml", EPG));
        let mut sent = 0;
        run(&req, &AtomicBool::new(true), |_| sent += 1);
        assert_eq!(sent, 0);
    }

    #[test]
    fn epg_is_filtered_by_playlist_channels_and_time() {
        // 2024-02-29 12:00 UTC.
        let now = 1_709_208_000;
        let epg_xml = r#"<tv>
            <channel id="a"><display-name>A</display-name></channel>
            <channel id="other"><display-name>Other</display-name></channel>
            <programme start="20240229113000" stop="20240229123000" channel="a"><title>Now</title></programme>
            <programme start="20240220120000" stop="20240220130000" channel="a"><title>Last week</title></programme>
            <programme start="20240229113000" stop="20240229123000" channel="other"><title>Elsewhere</title></programme>
        </tv>"#;
        let mut req = request(&temp_file("f.m3u", PLAYLIST), &temp_file("f.xml", epg_xml));
        req.now = now;
        let ev = events(&req);
        let epg = &epg_of(&ev).as_ref().unwrap().as_ref().unwrap().data;
        assert_eq!(epg.channels.len(), 1);
        let titles: Vec<_> = epg.programmes.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Now"]);
    }

    #[test]
    fn archive_keeps_guide_history_as_deep_as_the_playlist_archive() {
        // 2024-02-29 12:00 UTC.
        let now = 1_709_208_000;
        let epg_xml = r#"<tv>
            <channel id="a"><display-name>A</display-name></channel>
            <programme start="20240229113000" stop="20240229123000" channel="a"><title>Now</title></programme>
            <programme start="20240227120000" stop="20240227130000" channel="a"><title>Two days ago</title></programme>
            <programme start="20240220120000" stop="20240220130000" channel="a"><title>Last week</title></programme>
        </tv>"#;
        let playlist = "#EXTM3U\n#EXTINF:-1 tvg-id=\"a\" catchup=\"shift\" catchup-days=\"3\",A\nhttp://example.com/a\n";
        let titles = |req: &Request| -> Vec<String> {
            let ev = events(req);
            let epg = &epg_of(&ev).as_ref().unwrap().as_ref().unwrap().data;
            epg.programmes.iter().map(|p| p.title.clone()).collect()
        };
        let mut req = request(&temp_file("r.m3u", playlist), &temp_file("r.xml", epg_xml));
        req.now = now;
        assert_eq!(titles(&req), ["Now"], "archive off");
        req.archive_days = Some(0); // the depth comes from the loaded playlist
        assert_eq!(titles(&req), ["Now", "Two days ago"]);
        // Playlist not reloaded: the app's depth counts.
        req.playlist_fetch = Fetch::Keep;
        req.archive_days = Some(10);
        assert_eq!(titles(&req), ["Now", "Two days ago", "Last week"]);
    }

    #[test]
    fn archive_history_is_capped() {
        assert_eq!(keep_past_secs(None), EPG_KEEP_PAST_SECS);
        assert_eq!(keep_past_secs(Some(0)), EPG_KEEP_PAST_SECS);
        assert_eq!(keep_past_secs(Some(2)), 2 * 24 * 3600);
        assert_eq!(
            keep_past_secs(Some(365)),
            i64::from(ARCHIVE_MAX_DAYS) * 24 * 3600
        );
    }

    #[test]
    fn saved_copy_policy() {
        let day = REFRESH_SECS;
        let daily = Fetch::Policy(UpdatePolicy::Daily);
        let never = Fetch::Policy(UpdatePolicy::Never);
        let start = Fetch::Policy(UpdatePolicy::OnStart);
        assert!(use_saved_copy(daily, Some(0), day - 1));
        assert!(!use_saved_copy(daily, Some(0), day));
        assert!(use_saved_copy(never, Some(0), 100 * day));
        assert!(!use_saved_copy(start, Some(0), 1));
        assert!(!use_saved_copy(Fetch::Download, Some(0), 1));
        for fetch in [daily, never, start] {
            assert!(!use_saved_copy(fetch, None, 0), "nothing saved yet");
        }
    }

    #[test]
    fn updates_while_running_are_daily_unless_never() {
        let day = REFRESH_SECS;
        assert!(update_due(UpdatePolicy::Daily, Some(0), day));
        assert!(update_due(UpdatePolicy::OnStart, Some(0), day));
        assert!(!update_due(UpdatePolicy::Daily, Some(0), day - 1));
        assert!(!update_due(UpdatePolicy::Never, Some(0), 10 * day));
        assert!(
            !update_due(UpdatePolicy::Daily, None, 10 * day),
            "local file"
        );
    }

    #[test]
    fn download_is_saved_and_reused_without_the_server() {
        let cache = temp_dir("reuse");
        let url = serve(PLAYLIST, 1);
        let mut req = request(&url, "");
        req.cache_dir = Some(cache.clone());
        req.now = unix_now();
        let ev = events(&req);
        let first = playlist_of(&ev).as_ref().unwrap();
        assert_eq!(first.downloaded_at, Some(req.now));
        assert!(!first.from_saved);
        let saved: Vec<_> = std::fs::read_dir(&cache).unwrap().collect();
        assert_eq!(saved.len(), 1, "one saved copy, no leftovers");

        // The server is gone: "daily" takes the fresh copy without asking.
        req.playlist_fetch = Fetch::Policy(UpdatePolicy::Daily);
        let ev = events(&req);
        let again = playlist_of(&ev).as_ref().unwrap();
        assert_eq!(again.data.channels.len(), 1);
        assert!(again.from_saved && again.stale.is_none());
        assert!(
            again
                .downloaded_at
                .is_some_and(|t| (t - req.now).abs() < 60)
        );
        std::fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn failed_download_falls_back_to_the_saved_copy() {
        let cache = temp_dir("fallback");
        let mut req = request(DEAD_URL, "");
        req.cache_dir = Some(cache.clone());
        let saved = cache.join(cache::file_name(DEAD_URL, "playlist"));
        std::fs::write(&saved, PLAYLIST).unwrap();
        for fetch in [Fetch::Download, Fetch::Policy(UpdatePolicy::OnStart)] {
            req.playlist_fetch = fetch;
            let ev = events(&req);
            let fetched = playlist_of(&ev).as_ref().unwrap();
            assert_eq!(fetched.data.channels.len(), 1);
            assert!(fetched.from_saved);
            let why = fetched.stale.as_deref().unwrap();
            assert!(!why.contains("fakekey"), "leaked: {why}");
        }
        std::fs::remove_dir_all(&cache).unwrap();
        // Nothing saved: the error itself.
        assert!(playlist_of(&events(&req)).is_err());
    }

    #[test]
    fn copies_of_sources_no_longer_used_are_deleted() {
        let cache = temp_dir("unused");
        let old = cache.join(cache::file_name("http://example.com/old.xml", "epg"));
        std::fs::write(&old, EPG).unwrap();
        let current = cache.join(cache::file_name(DEAD_URL, "playlist"));
        std::fs::write(&current, PLAYLIST).unwrap();
        let mut req = request(DEAD_URL, "");
        req.cache_dir = Some(cache.clone());
        events(&req);
        assert!(!old.exists(), "the old guide's copy is gone");
        assert!(current.exists(), "the playlist's copy stays");

        // A cancelled load leaves everything alone.
        std::fs::write(&old, EPG).unwrap();
        run(&req, &AtomicBool::new(true), |_| {});
        assert!(old.exists());
        std::fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn a_bad_download_does_not_replace_the_saved_copy() {
        let cache = temp_dir("bad");
        let url = serve("<html>maintenance</html>", 1);
        let mut req = request(&url, "");
        req.cache_dir = Some(cache.clone());
        let saved = cache.join(cache::file_name(&url, "playlist"));
        std::fs::write(&saved, PLAYLIST).unwrap();
        let ev = events(&req);
        let fetched = playlist_of(&ev).as_ref().unwrap();
        assert_eq!(fetched.stale.as_deref(), Some("missing #EXTM3U header"));
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), PLAYLIST);
        assert!(!part_path(&saved).exists());
        std::fs::remove_dir_all(&cache).unwrap();
    }

    fn unix_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[test]
    fn channel_filter_covers_ids_names_and_tvg_names() {
        let playlist = m3u::parse(
            "#EXTM3U\n#EXTINF:-1 tvg-id=\"ID.One\" tvg-name=\"Alt Name\",Shown Name\nhttp://example.com/1\n",
        )
        .unwrap();
        let f = channel_filter(&playlist);
        assert!(f.ids.contains("id.one"));
        assert!(f.names.contains("alt name"));
        assert!(f.names.contains("shown name"));

        let feed =
            m3u::parse("#EXTM3U\n#EXTINF:-1 tvg-id=\"La1.es@SD\",La 1\nhttp://example.com/1\n")
                .unwrap();
        let f = channel_filter(&feed);
        assert!(f.ids.contains("la1.es@sd") && f.ids.contains("la1.es"));
    }

    #[test]
    fn empty_playlist_source_is_an_error() {
        assert!(playlist_of(&events(&request("", ""))).is_err());
    }

    #[test]
    fn invalid_playlist_reports_parse_error() {
        let ev = events(&request(&temp_file("bad.m3u", "not a playlist"), ""));
        assert_eq!(
            playlist_of(&ev).as_ref().unwrap_err(),
            "missing #EXTM3U header"
        );
    }

    #[test]
    fn broken_epg_does_not_break_playlist() {
        let ev = events(&request(
            &temp_file("c2.m3u", PLAYLIST),
            &temp_file("bad.xml", "<tv><a></tv>"),
        ));
        assert!(playlist_of(&ev).is_ok());
        assert!(epg_of(&ev).as_ref().unwrap().is_err());
    }

    #[test]
    fn url_errors_do_not_leak_the_url() {
        let ev = events(&request(DEAD_URL, ""));
        let msg = playlist_of(&ev).as_ref().unwrap_err();
        assert!(!msg.contains("fakekey"), "leaked: {msg}");
    }
}
