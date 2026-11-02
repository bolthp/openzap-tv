//! UI-independent logic for browsing a playlist: grouping, filtering, formatting.

use std::collections::BTreeSet;

use crate::epg::Programme;
use crate::m3u::Playlist;

/// Sorted, de-duplicated list of group names (channels without a group are skipped).
pub fn groups(playlist: &Playlist) -> Vec<String> {
    let set: BTreeSet<&str> = playlist
        .channels
        .iter()
        .filter_map(|c| c.group_title.as_deref())
        .collect();
    set.into_iter().map(String::from).collect()
}

/// Indices of channels whose name contains `query` (case-insensitive)
/// and, if given, that belong to `group`. Indices keep playlist order.
///
/// Returning indices instead of references lets the UI store the result
/// without borrowing the playlist.
pub fn filter_channels(playlist: &Playlist, query: &str, group: Option<&str>) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    playlist
        .channels
        .iter()
        .enumerate()
        .filter(|(_, c)| group.is_none_or(|g| c.group_title.as_deref() == Some(g)))
        .filter(|(_, c)| query.is_empty() || c.name.to_lowercase().contains(&query))
        .map(|(i, _)| i)
        .collect()
}

/// Fraction of the programme that has elapsed at `at`, clamped to `0.0..=1.0`.
pub fn progress(p: &Programme, at: i64) -> f32 {
    let total = p.stop - p.start;
    if total <= 0 {
        return 0.0;
    }
    ((at - p.start) as f32 / total as f32).clamp(0.0, 1.0)
}

/// Human-readable duration: `<1 min`, `25 min`, `2 h`, `1 h 05 min`.
pub fn format_duration(secs: i64) -> String {
    let mins = secs.max(0) / 60;
    match (mins / 60, mins % 60) {
        (0, 0) => "<1 min".to_string(),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m:02} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m3u::Channel;

    fn ch(name: &str, group: Option<&str>) -> Channel {
        Channel {
            name: name.into(),
            group_title: group.map(String::from),
            ..Channel::default()
        }
    }

    fn sample() -> Playlist {
        Playlist {
            channels: vec![
                ch("Example News", Some("News")),
                ch("Example Film", Some("Movies")),
                ch("Sport One", Some("Sports")),
                ch("Film Classics", Some("Movies")),
                ch("Ungrouped", None),
            ],
        }
    }

    fn prog(start: i64, stop: i64) -> Programme {
        Programme {
            channel: "c".into(),
            start,
            stop,
            title: "T".into(),
            desc: None,
            category: None,
        }
    }

    #[test]
    fn groups_sorted_unique() {
        assert_eq!(groups(&sample()), ["Movies", "News", "Sports"]);
    }

    #[test]
    fn empty_filter_returns_all() {
        assert_eq!(filter_channels(&sample(), "", None), [0, 1, 2, 3, 4]);
        assert_eq!(filter_channels(&sample(), "   ", None), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn query_is_case_insensitive_substring() {
        assert_eq!(filter_channels(&sample(), "FILM", None), [1, 3]);
    }

    #[test]
    fn group_filter() {
        assert_eq!(filter_channels(&sample(), "", Some("Movies")), [1, 3]);
        assert!(filter_channels(&sample(), "", Some("Nope")).is_empty());
    }

    #[test]
    fn query_and_group_combine() {
        assert_eq!(filter_channels(&sample(), "classics", Some("Movies")), [3]);
        assert!(filter_channels(&sample(), "news", Some("Movies")).is_empty());
    }

    #[test]
    fn non_ascii_query() {
        let pl = Playlist {
            channels: vec![ch("Первый Канал", None)],
        };
        assert_eq!(filter_channels(&pl, "первый", None), [0]);
    }

    #[test]
    fn progress_values() {
        let p = prog(100, 200);
        assert_eq!(progress(&p, 50), 0.0);
        assert_eq!(progress(&p, 150), 0.5);
        assert_eq!(progress(&p, 300), 1.0);
        assert_eq!(progress(&prog(10, 10), 10), 0.0);
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(-5), "<1 min");
        assert_eq!(format_duration(59), "<1 min");
        assert_eq!(format_duration(25 * 60 + 30), "25 min");
        assert_eq!(format_duration(2 * 3600), "2 h");
        assert_eq!(format_duration(3600 + 5 * 60), "1 h 05 min");
    }
}
