//! Audio tracks of the playing stream, read from mpv's `track-list`.

use crate::streaminfo::{channels_label, codec_label};

/// Upper bound on tracks read, against a bogus `track-list/count`.
const MAX_TRACKS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    /// mpv's track id (the value for the `aid` property).
    pub id: i64,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub codec: Option<String>,
    pub channels: Option<u32>,
    pub selected: bool,
}

impl AudioTrack {
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
pub fn audio_tracks(get: impl Fn(&str) -> Option<String>) -> Vec<AudioTrack> {
    let count = get("track-list/count")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_TRACKS);
    (0..count)
        .filter_map(|i| {
            let field = |name: &str| {
                get(&format!("track-list/{i}/{name}")).filter(|v| !v.trim().is_empty())
            };
            if field("type")? != "audio" {
                return None;
            }
            Some(AudioTrack {
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
pub fn next_track(tracks: &[AudioTrack]) -> Option<&AudioTrack> {
    if tracks.len() < 2 {
        return None;
    }
    let current = tracks.iter().position(|t| t.selected).unwrap_or(0);
    tracks.get((current + 1) % tracks.len())
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

    fn tracks(pairs: &[(&str, &str)]) -> Vec<AudioTrack> {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        audio_tracks(|name| map.get(name).map(|v| v.to_string()))
    }

    fn two_audio() -> Vec<AudioTrack> {
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
    fn language_names() {
        assert_eq!(language_name("RUS"), "Russian");
        assert_eq!(language_name("xyz"), "xyz");
    }
}
