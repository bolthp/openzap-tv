//! Archive (catch-up): playing past programmes that the provider keeps.
//!
//! Recordings live on the provider's server; the player only builds a
//! different URL for the time wanted. Playlists describe the archive with
//! `catchup` (the URL scheme), `catchup-days` / `tvg-rec` (how far back)
//! and `catchup-source` (a URL template), per channel or in the
//! `#EXTM3U` header as defaults.

use std::fmt;

use crate::clock::civil_from_days;
use crate::m3u::Playlist;
use crate::mask::mask_url;

/// Days of archive assumed when a playlist names a scheme but no depth.
pub const DEFAULT_DAYS: u32 = 5;

/// How the URL of a past programme is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `catchup-source` is the whole URL, with placeholders.
    Default,
    /// `catchup-source` (with placeholders) is appended to the live URL.
    Append,
    /// `?utc=<start>&lutc=<now>` is appended to the live URL.
    Shift,
    /// Flussonic paths: `index.m3u8` → `index-<start>-<duration>.m3u8`,
    /// `mpegts` → `timeshift_abs-<start>.ts`.
    Flussonic,
}

/// A channel's archive.
#[derive(Clone, PartialEq, Eq)]
pub struct Catchup {
    pub mode: Mode,
    /// How many days back programmes can be played.
    pub days: u32,
    /// URL template for [`Mode::Default`] and [`Mode::Append`].
    pub source: Option<String>,
}

/// Archive attributes as written in the playlist (not yet checked).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attrs {
    pub mode: Option<String>,
    pub days: Option<String>,
    pub source: Option<String>,
}

impl Attrs {
    /// Takes one `key="value"` attribute; false if it is not about the archive.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        let value = Some(value.trim().to_string()).filter(|v| !v.is_empty());
        let slot = match key {
            "catchup" | "catchup-type" => &mut self.mode,
            // `tvg-rec` is OTT-Play's name, `timeshift` an older one.
            "catchup-days" | "tvg-rec" | "timeshift" => &mut self.days,
            "catchup-source" => &mut self.source,
            _ => return false,
        };
        if value.is_some() {
            *slot = value;
        }
        true
    }

    /// The channel's own attributes, falling back to the playlist's defaults.
    pub fn or(self, defaults: &Attrs) -> Attrs {
        Attrs {
            mode: self.mode.or_else(|| defaults.mode.clone()),
            days: self.days.or_else(|| defaults.days.clone()),
            source: self.source.or_else(|| defaults.source.clone()),
        }
    }

    /// The archive these attributes describe for a channel at `url`, if any.
    /// Unknown schemes give `None` rather than a guess.
    pub fn resolve(&self, url: &str) -> Option<Catchup> {
        let days = self.days.as_deref().and_then(|d| d.parse::<u32>().ok());
        if days == Some(0) {
            return None;
        }
        let source = self.source.clone();
        let mode = match self.mode.as_deref().map(str::to_ascii_lowercase) {
            Some(m) => match m.as_str() {
                "default" if source.is_some() => Mode::Default,
                "default" | "shift" | "timeshift" | "archive" => Mode::Shift,
                "append" if source.is_some() => Mode::Append,
                "flussonic" | "flussonic-hls" | "flussonic-ts" | "fs" => Mode::Flussonic,
                _ => return None,
            },
            // Only a depth (e.g. OTT-Play's `tvg-rec="7"`): OTT-Play then
            // uses the shift scheme. Guessing Flussonic from `index.m3u8`
            // is wrong for providers whose servers just send the live
            // stream for an unknown path.
            None if days.is_some() => {
                if source.is_some() {
                    Mode::Default
                } else {
                    Mode::Shift
                }
            }
            None => return None,
        };
        if mode == Mode::Flussonic && flussonic_url(url, 0, 0).is_none() {
            return None;
        }
        Some(Catchup {
            mode,
            days: days.unwrap_or(DEFAULT_DAYS),
            source,
        })
    }
}

// Manual `Debug`: the template may contain access keys.
impl fmt::Debug for Catchup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Catchup")
            .field("mode", &self.mode)
            .field("days", &self.days)
            .field("source", &self.source.as_deref().map(mask_url))
            .finish()
    }
}

impl Catchup {
    /// Whether a programme starting at `start` can be played from the
    /// archive at `now`: it has begun, and not longer ago than the archive keeps.
    pub fn covers(&self, start: i64, now: i64) -> bool {
        start <= now && now - start <= i64::from(self.days) * 86_400
    }

    /// URL of the programme `start..stop` (Unix seconds) of the channel
    /// whose live URL is `live`. `None` if the scheme does not fit the URL.
    pub fn url(&self, live: &str, start: i64, stop: i64, now: i64) -> Option<String> {
        let times = Times { start, stop, now };
        match self.mode {
            Mode::Default => Some(expand(self.source.as_deref()?, &times)),
            Mode::Append => Some(format!("{live}{}", expand(self.source.as_deref()?, &times))),
            Mode::Shift => {
                let sep = if live.contains('?') { '&' } else { '?' };
                Some(format!("{live}{sep}utc={start}&lutc={now}"))
            }
            Mode::Flussonic => flussonic_url(live, start, stop - start),
        }
    }
}

/// The deepest archive in the playlist, in days (0: no archive).
pub fn max_days(playlist: &Playlist) -> u32 {
    playlist
        .channels
        .iter()
        .filter_map(|c| c.catchup.as_ref().map(|a| a.days))
        .max()
        .unwrap_or(0)
}

/// One line about the playlist's archive for the debug log (no URLs):
/// "archive: 12 of 40 channels (shift 10, flussonic 2), up to 7 days".
pub fn summary(playlist: &Playlist) -> String {
    let with: Vec<&Catchup> = playlist
        .channels
        .iter()
        .filter_map(|c| c.catchup.as_ref())
        .collect();
    if with.is_empty() {
        return "archive: none in the playlist".into();
    }
    let modes = [
        (Mode::Shift, "shift"),
        (Mode::Flussonic, "flussonic"),
        (Mode::Default, "default"),
        (Mode::Append, "append"),
    ]
    .iter()
    .filter_map(|&(mode, name)| {
        let n = with.iter().filter(|c| c.mode == mode).count();
        (n > 0).then(|| format!("{name} {n}"))
    })
    .collect::<Vec<_>>()
    .join(", ");
    format!(
        "archive: {} of {} channels ({modes}), up to {} days",
        with.len(),
        playlist.channels.len(),
        max_days(playlist)
    )
}

/// Splits a URL into the part up to the last path segment, that segment,
/// and the rest (`?query`), or `None` if there is no path.
fn split_last_segment(url: &str) -> Option<(&str, &str, &str)> {
    let query_at = url.find('?').unwrap_or(url.len());
    let (path, query) = url.split_at(query_at);
    let host_end = path.find("://").map_or(0, |i| i + 3);
    let slash = path[host_end..].rfind('/')? + host_end;
    Some((&path[..=slash], &path[slash + 1..], query))
}

fn flussonic_url(live: &str, start: i64, duration: i64) -> Option<String> {
    let (base, last, query) = split_last_segment(live)?;
    if last == "mpegts" {
        return Some(format!("{base}timeshift_abs-{start}.ts{query}"));
    }
    let name = last.strip_suffix(".m3u8").filter(|n| !n.is_empty())?;
    Some(format!("{base}{name}-{start}-{duration}.m3u8{query}"))
}

struct Times {
    start: i64,
    stop: i64,
    now: i64,
}

/// Fills in a `catchup-source` template. Placeholders, written `{name}` or
/// `${name}`: `utc`/`start`, `utcend`/`end`, `lutc`/`now`/`timestamp`
/// (Unix seconds; with `:YmdHMS`-style formats as UTC dates), `duration`
/// and `offset` (seconds; `{duration:60}` divides), `Y m d H M S` (start,
/// UTC). Unknown placeholders are left as they are.
fn expand(template: &str, t: &Times) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(len) = rest[open..].find('}') else {
            break;
        };
        let inner = &rest[open + 1..open + len];
        let (before, dollar) = match rest[..open].strip_suffix('$') {
            Some(b) => (b, "$"),
            None => (&rest[..open], ""),
        };
        out.push_str(before);
        match placeholder(inner, t) {
            Some(value) => out.push_str(&value),
            None => {
                out.push_str(dollar);
                out.push_str(&rest[open..=open + len]);
            }
        }
        rest = &rest[open + len + 1..];
    }
    out.push_str(rest);
    out
}

fn placeholder(inner: &str, t: &Times) -> Option<String> {
    let (name, arg) = match inner.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (inner, None),
    };
    let time = match name {
        "utc" | "start" => Some(t.start),
        "utcend" | "end" => Some(t.stop),
        "lutc" | "now" | "timestamp" => Some(t.now),
        _ => None,
    };
    if let Some(time) = time {
        return Some(match arg {
            Some(format) => format_utc(time, format),
            None => time.to_string(),
        });
    }
    let secs = match name {
        "duration" => t.stop - t.start,
        "offset" => t.now - t.start,
        "Y" | "m" | "d" | "H" | "M" | "S" if arg.is_none() => {
            return Some(format_utc(t.start, name));
        }
        _ => return None,
    };
    let divisor = match arg {
        Some(a) => a.parse::<i64>().ok().filter(|&d| d > 0)?,
        None => 1,
    };
    Some((secs / divisor).to_string())
}

/// Formats a Unix time as a UTC date: `Y m d H M S` are replaced, other
/// characters are kept (`Y-m-d:H-M` → `2026-11-12:08-05`).
fn format_utc(unix: i64, format: &str) -> String {
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    let mut out = String::new();
    for c in format.chars() {
        match c {
            'Y' => out.push_str(&format!("{year:04}")),
            'm' => out.push_str(&format!("{month:02}")),
            'd' => out.push_str(&format!("{day:02}")),
            'H' => out.push_str(&format!("{:02}", secs / 3600)),
            'M' => out.push_str(&format!("{:02}", secs / 60 % 60)),
            'S' => out.push_str(&format!("{:02}", secs % 60)),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-12 08:05:09 UTC and an hour-long programme.
    const START: i64 = 1_794_470_709;
    const STOP: i64 = START + 3600;
    const NOW: i64 = START + 7200;

    fn attrs(mode: Option<&str>, days: Option<&str>, source: Option<&str>) -> Attrs {
        Attrs {
            mode: mode.map(String::from),
            days: days.map(String::from),
            source: source.map(String::from),
        }
    }

    fn url(a: Attrs, live: &str) -> Option<String> {
        a.resolve(live)?.url(live, START, STOP, NOW)
    }

    #[test]
    fn shift_appends_query() {
        let a = attrs(Some("shift"), Some("3"), None);
        assert_eq!(
            url(a.clone(), "http://example.com/ch/index.m3u8").as_deref(),
            Some(&*format!(
                "http://example.com/ch/index.m3u8?utc={START}&lutc={NOW}"
            ))
        );
        assert_eq!(
            url(a, "http://example.com/ch.m3u8?token=fakekey").as_deref(),
            Some(&*format!(
                "http://example.com/ch.m3u8?token=fakekey&utc={START}&lutc={NOW}"
            ))
        );
    }

    #[test]
    fn flussonic_rewrites_the_path() {
        let a = attrs(Some("flussonic"), None, None);
        assert_eq!(
            url(a.clone(), "http://example.com/ch1/index.m3u8?token=fakekey").as_deref(),
            Some(&*format!(
                "http://example.com/ch1/index-{START}-3600.m3u8?token=fakekey"
            ))
        );
        assert_eq!(
            url(a.clone(), "http://example.com/ch1/mpegts?token=fakekey").as_deref(),
            Some(&*format!(
                "http://example.com/ch1/timeshift_abs-{START}.ts?token=fakekey"
            ))
        );
        // Not a Flussonic URL: no archive rather than a broken link.
        assert_eq!(a.resolve("http://example.com/ch1/stream.ts"), None);
    }

    #[test]
    fn templates_fill_in_times() {
        let a = attrs(
            Some("default"),
            None,
            Some("http://example.com/arc/{utc}/{utcend}/{lutc}/{duration}/{duration:60}/{offset}"),
        );
        assert_eq!(
            url(a, "http://example.com/live").as_deref(),
            Some(&*format!(
                "http://example.com/arc/{START}/{STOP}/{NOW}/3600/60/7200"
            ))
        );
        let a = attrs(
            Some("append"),
            None,
            Some("?start=${start}&date={Y}{m}{d}-{H}{M}{S}&end={utcend:Y-m-d:H-M}"),
        );
        assert_eq!(
            url(a, "http://example.com/live").as_deref(),
            Some(&*format!(
                "http://example.com/live?start={START}&date=20261112-080509&end=2026-11-12:09-05"
            ))
        );
    }

    #[test]
    fn unknown_placeholders_stay() {
        let t = Times {
            start: 1,
            stop: 2,
            now: 3,
        };
        assert_eq!(expand("a{x}${y}{utc}{", &t), "a{x}${y}1{");
        assert_eq!(expand("{duration:0}", &t), "{duration:0}");
    }

    #[test]
    fn depth_alone_means_shift() {
        let rec = attrs(None, Some("7"), None);
        let c = rec
            .resolve("http://example.com/iptv/fakekey/123/index.m3u8")
            .unwrap();
        assert_eq!((c.mode, c.days), (Mode::Shift, 7));
        assert_eq!(
            c.url(
                "http://example.com/iptv/fakekey/123/index.m3u8",
                START,
                STOP,
                NOW
            )
            .as_deref(),
            Some(&*format!(
                "http://example.com/iptv/fakekey/123/index.m3u8?utc={START}&lutc={NOW}"
            ))
        );
        let with_source = attrs(None, Some("7"), Some("http://example.com/{utc}"));
        assert_eq!(with_source.resolve("x").unwrap().mode, Mode::Default);
    }

    #[test]
    fn missing_or_unknown_gives_no_archive() {
        assert_eq!(Attrs::default().resolve("http://example.com/a"), None);
        assert_eq!(attrs(Some("shift"), Some("0"), None).resolve("x"), None);
        assert_eq!(attrs(Some("magic"), Some("3"), None).resolve("x"), None);
        assert_eq!(attrs(Some("append"), None, None).resolve("x"), None);
    }

    #[test]
    fn scheme_without_depth_uses_default_days() {
        let c = attrs(Some("shift"), Some("x"), None).resolve("x").unwrap();
        assert_eq!(c.days, DEFAULT_DAYS);
    }

    #[test]
    fn covers_started_programmes_within_depth() {
        let c = attrs(Some("shift"), Some("1"), None).resolve("x").unwrap();
        assert!(c.covers(NOW - 60, NOW));
        assert!(c.covers(NOW, NOW));
        assert!(!c.covers(NOW + 60, NOW), "not started yet");
        assert!(c.covers(NOW - 86_400, NOW));
        assert!(!c.covers(NOW - 86_401, NOW), "older than the archive");
    }

    #[test]
    fn channel_attributes_win_over_header_defaults() {
        let mut header = Attrs::default();
        assert!(header.set("catchup", "shift"));
        assert!(header.set("catchup-days", "3"));
        assert!(!header.set("tvg-id", "x"));
        let mut own = Attrs::default();
        own.set("tvg-rec", "7");
        own.set("catchup-source", " ");
        let merged = own.or(&header);
        assert_eq!(merged, attrs(Some("shift"), Some("7"), None));
    }

    #[test]
    fn debug_output_masks_the_template() {
        let a = attrs(
            Some("default"),
            None,
            Some("http://example.com/{utc}?token=fakekey123"),
        );
        let dbg = format!("{:?}", a.resolve("x").unwrap());
        assert!(!dbg.contains("fakekey123"), "{dbg}");
    }

    #[test]
    fn summary_counts_channels_by_scheme() {
        let pl = crate::m3u::parse(
            "#EXTM3U\n\
             #EXTINF:-1 catchup=\"shift\" catchup-days=\"3\",A\nhttp://example.com/a\n\
             #EXTINF:-1 catchup=\"flussonic\" tvg-rec=\"7\",B\nhttp://example.com/b/index.m3u8\n\
             #EXTINF:-1,C\nhttp://example.com/c\n",
        )
        .unwrap();
        assert_eq!(
            summary(&pl),
            "archive: 2 of 3 channels (shift 1, flussonic 1), up to 7 days"
        );
        assert_eq!(
            summary(&Playlist::default()),
            "archive: none in the playlist"
        );
    }

    #[test]
    fn formats_dates_before_1970() {
        assert_eq!(format_utc(-1, "Y-m-d H:M:S"), "1969-12-31 23:59:59");
    }
}
