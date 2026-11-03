//! Audio and subtitle tracks of the playing stream, from mpv's `track-list`.

use crate::streaminfo::{channels_label, codec_label};

/// Upper bound on tracks read, against a bogus `track-list/count`.
const MAX_TRACKS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// mpv's track id (the value for the `aid` / `sid` property).
    pub id: i64,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub codec: Option<String>,
    pub channels: Option<u32>,
    pub selected: bool,
}

impl Track {
    /// E.g. "English · AC-3 5.1", or "Track 2 · AAC stereo".
    pub fn label(&self) -> String {
        let name = match (&self.title, &self.lang) {
            (Some(title), _) => title.clone(),
            (None, Some(lang)) => language_name(lang),
            (None, None) => format!("Track {}", self.id),
        };
        let format: Vec<String> = [
            self.codec.as_deref().map(codec_label),
            self.channels.map(channels_label),
        ]
        .into_iter()
        .flatten()
        .collect();
        if format.is_empty() {
            name
        } else {
            format!("{name} · {}", format.join(" "))
        }
    }
}

/// The audio tracks, given a property lookup (`get(name)` returns mpv's
/// text value, or `None` when unavailable).
pub fn audio_tracks(get: impl Fn(&str) -> Option<String>) -> Vec<Track> {
    tracks_of("audio", get)
}

/// The subtitle tracks (same lookup as [`audio_tracks`]).
pub fn subtitle_tracks(get: impl Fn(&str) -> Option<String>) -> Vec<Track> {
    tracks_of("sub", get)
}

/// Tracks of mpv type `kind` ("audio", "sub", "video").
fn tracks_of(kind: &str, get: impl Fn(&str) -> Option<String>) -> Vec<Track> {
    let count = get("track-list/count")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_TRACKS);
    (0..count)
        .filter_map(|i| {
            let field = |name: &str| {
                get(&format!("track-list/{i}/{name}")).filter(|v| !v.trim().is_empty())
            };
            if field("type")? != kind {
                return None;
            }
            Some(Track {
                id: field("id")?.parse().ok()?,
                lang: field("lang"),
                title: field("title"),
                codec: field("codec"),
                channels: field("demux-channel-count").and_then(|v| v.parse().ok()),
                selected: field("selected").as_deref() == Some("yes"),
            })
        })
        .collect()
}

/// The track after the selected one, wrapping around; `None` if there is
/// nothing to switch to.
pub fn next_track(tracks: &[Track]) -> Option<&Track> {
    if tracks.len() < 2 {
        return None;
    }
    let current = tracks.iter().position(|t| t.selected).unwrap_or(0);
    tracks.get((current + 1) % tracks.len())
}

/// The subtitle after the selected one, with "off" (`None`) in the cycle:
/// off → first → … → last → off.
pub fn next_subtitle(tracks: &[Track]) -> Option<i64> {
    match tracks.iter().position(|t| t.selected) {
        None => tracks.first().map(|t| t.id),
        Some(i) => tracks.get(i + 1).map(|t| t.id),
    }
}

/// Readable names for common language codes; others are shown as given.
fn language_name(code: &str) -> String {
    let name = match code.to_ascii_lowercase().as_str() {
        "ru" | "rus" => "Russian",
        "en" | "eng" => "English",
        "uk" | "ukr" => "Ukrainian",
        "de" | "deu" | "ger" => "German",
        "fr" | "fra" | "fre" => "French",
        "es" | "spa" => "Spanish",
        "it" | "ita" => "Italian",
        "pl" | "pol" => "Polish",
        "tr" | "tur" => "Turkish",
        "be" | "bel" => "Belarusian",
        "kk" | "kaz" => "Kazakh",
        "und" => "Unknown language",
        _ => return code.to_string(),
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tracks(pairs: &[(&str, &str)]) -> Vec<Track> {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        audio_tracks(|name| map.get(name).map(|v| v.to_string()))
    }

    fn subs(pairs: &[(&str, &str)]) -> Vec<Track> {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        subtitle_tracks(|name| map.get(name).map(|v| v.to_string()))
    }

    fn two_audio() -> Vec<Track> {
        tracks(&[
            ("track-list/count", "3"),
            ("track-list/0/type", "video"),
            ("track-list/0/id", "1"),
            ("track-list/1/type", "audio"),
            ("track-list/1/id", "1"),
            ("track-list/1/lang", "rus"),
            ("track-list/1/codec", "aac"),
            ("track-list/1/demux-channel-count", "2"),
            ("track-list/1/selected", "yes"),
            ("track-list/2/type", "audio"),
            ("track-list/2/id", "2"),
            ("track-list/2/lang", "eng"),
            ("track-list/2/title", "Original"),
            ("track-list/2/codec", "eac3"),
            ("track-list/2/demux-channel-count", "6"),
            ("track-list/2/selected", "no"),
        ])
    }

    #[test]
    fn reads_only_audio_tracks() {
        let t = two_audio();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].id, 1);
        assert!(t[0].selected && !t[1].selected);
        assert_eq!(t[0].label(), "Russian · AAC stereo");
        // A title wins over the language.
        assert_eq!(t[1].label(), "Original · E-AC-3 5.1");
    }

    #[test]
    fn minimal_and_broken_entries() {
        let t = tracks(&[
            ("track-list/count", "3"),
            ("track-list/0/type", "audio"),
            ("track-list/0/id", "4"),
            ("track-list/1/type", "audio"), // no id: skipped
            ("track-list/2/type", "audio"),
            ("track-list/2/id", "x"), // bad id: skipped
        ]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].label(), "Track 4");
        assert!(tracks(&[]).is_empty());
        assert!(tracks(&[("track-list/count", "-1")]).is_empty());
    }

    #[test]
    fn count_is_capped() {
        let asked = std::cell::Cell::new(0);
        audio_tracks(|name| {
            asked.set(asked.get() + 1);
            (name == "track-list/count").then(|| "1000000".to_string())
        });
        assert!(asked.get() <= 1 + MAX_TRACKS);
    }

    #[test]
    fn next_track_wraps_and_needs_two() {
        let t = two_audio();
        assert_eq!(next_track(&t).map(|t| t.id), Some(2));
        let mut swapped = t.clone();
        swapped[0].selected = false;
        swapped[1].selected = true;
        assert_eq!(next_track(&swapped).map(|t| t.id), Some(1));
        assert_eq!(next_track(&t[..1]), None);
    }

    #[test]
    fn subtitles_are_read_and_cycle_through_off() {
        let subs = subs(&[
            ("track-list/count", "3"),
            ("track-list/0/type", "audio"),
            ("track-list/0/id", "1"),
            ("track-list/1/type", "sub"),
            ("track-list/1/id", "1"),
            ("track-list/1/lang", "eng"),
            ("track-list/1/codec", "subrip"),
            ("track-list/2/type", "sub"),
            ("track-list/2/id", "2"),
            ("track-list/2/codec", "dvb_subtitle"),
            ("track-list/2/selected", "yes"),
        ]);
        let map: Vec<i64> = subs.iter().map(|t| t.id).collect();
        assert_eq!(map, [1, 2]);
        assert_eq!(subs[0].label(), "English · SRT");
        assert_eq!(subs[1].label(), "Track 2 · DVB");
        // 2 is selected: next is off; from off the first comes back.
        assert_eq!(next_subtitle(&subs), None);
        let mut off = subs.clone();
        off[1].selected = false;
        assert_eq!(next_subtitle(&off), Some(1));
        assert_eq!(next_subtitle(&[]), None);
    }

    #[test]
    fn language_names() {
        assert_eq!(language_name("RUS"), "Russian");
        assert_eq!(language_name("xyz"), "xyz");
    }
}
