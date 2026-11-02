//! Linking playlist channels to EPG data: matching and "now / next" lookup.

use crate::epg::{Epg, EpgChannel, Programme};
use crate::m3u::Channel;

/// What is on air at a given moment and what follows.
///
/// Holds references into the `Epg` — no data is copied. The lifetime `'a`
/// ties this struct to the `Epg` it was taken from.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NowNext<'a> {
    pub now: Option<&'a Programme>,
    pub next: Option<&'a Programme>,
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Finds the EPG channel for a playlist channel.
///
/// Order: `tvg-id` == EPG id, then `tvg-name`, then the display name against
/// EPG display names. All comparisons are case-insensitive.
pub fn epg_channel_for<'a>(channel: &Channel, epg: &'a Epg) -> Option<&'a EpgChannel> {
    if let Some(id) = channel.tvg_id.as_deref().map(norm)
        && let Some(found) = epg.channels.iter().find(|c| norm(&c.id) == id)
    {
        return Some(found);
    }
    let names = [channel.tvg_name.as_deref(), Some(channel.name.as_str())];
    names
        .into_iter()
        .flatten()
        .map(norm)
        .filter(|n| !n.is_empty())
        .find_map(|n| epg.channels.iter().find(|c| norm(&c.display_name) == n))
}

/// Current and next programme of the EPG channel `epg_id` at Unix time `at`.
///
/// If programmes overlap, the one that started last counts as "now".
pub fn now_next<'a>(epg: &'a Epg, epg_id: &str, at: i64) -> NowNext<'a> {
    let of_channel = || epg.programmes.iter().filter(|p| p.channel == epg_id);

    let now = of_channel()
        .filter(|p| p.start <= at && at < p.stop)
        .max_by_key(|p| p.start);
    // "Next" begins no earlier than the end of "now" (or after `at` if nothing is on air).
    let threshold = now.map_or(at, |p| p.stop.max(at));
    let next = of_channel()
        .filter(|p| p.start >= threshold && p.start > at)
        .min_by_key(|p| p.start);
    NowNext { now, next }
}

/// Convenience: match the channel, then look up now/next. Empty if no EPG match.
pub fn now_next_for<'a>(channel: &Channel, epg: &'a Epg, at: i64) -> NowNext<'a> {
    match epg_channel_for(channel, epg) {
        Some(c) => now_next(epg, &c.id, at),
        None => NowNext::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epg_channel(id: &str, name: &str) -> EpgChannel {
        EpgChannel {
            id: id.into(),
            display_name: name.into(),
            icon: None,
        }
    }

    fn prog(channel: &str, start: i64, stop: i64, title: &str) -> Programme {
        Programme {
            channel: channel.into(),
            start,
            stop,
            title: title.into(),
            desc: None,
            category: None,
        }
    }

    fn playlist_channel(name: &str, tvg_id: Option<&str>) -> Channel {
        Channel {
            name: name.into(),
            tvg_id: tvg_id.map(String::from),
            ..Channel::default()
        }
    }

    fn sample_epg() -> Epg {
        Epg {
            channels: vec![
                epg_channel("news.example", "Example News"),
                epg_channel("film.example", "Example Film"),
            ],
            programmes: vec![
                prog("news.example", 100, 200, "A"),
                prog("news.example", 200, 300, "B"),
                prog("news.example", 300, 400, "C"),
                prog("film.example", 150, 250, "F"),
            ],
        }
    }

    #[test]
    fn matches_by_tvg_id_case_insensitive() {
        let epg = sample_epg();
        let ch = playlist_channel("Whatever", Some("NEWS.Example"));
        assert_eq!(epg_channel_for(&ch, &epg).unwrap().id, "news.example");
    }

    #[test]
    fn falls_back_to_name() {
        let epg = sample_epg();
        let ch = playlist_channel("  example film ", None);
        assert_eq!(epg_channel_for(&ch, &epg).unwrap().id, "film.example");
    }

    #[test]
    fn falls_back_to_name_when_tvg_id_unknown() {
        let epg = sample_epg();
        let ch = playlist_channel("Example News", Some("nope"));
        assert_eq!(epg_channel_for(&ch, &epg).unwrap().id, "news.example");
    }

    #[test]
    fn matches_non_ascii_names() {
        let epg = Epg {
            channels: vec![epg_channel("1", "Первый Канал")],
            programmes: vec![],
        };
        let ch = playlist_channel("первый канал", None);
        assert!(epg_channel_for(&ch, &epg).is_some());
    }

    #[test]
    fn no_match_returns_none() {
        let epg = sample_epg();
        assert!(epg_channel_for(&playlist_channel("Other", None), &epg).is_none());
        assert!(epg_channel_for(&playlist_channel("", None), &epg).is_none());
    }

    #[test]
    fn now_and_next_in_the_middle() {
        let epg = sample_epg();
        let nn = now_next(&epg, "news.example", 250);
        assert_eq!(nn.now.unwrap().title, "B");
        assert_eq!(nn.next.unwrap().title, "C");
    }

    #[test]
    fn stop_time_is_exclusive() {
        let epg = sample_epg();
        let nn = now_next(&epg, "news.example", 200);
        assert_eq!(nn.now.unwrap().title, "B");
    }

    #[test]
    fn before_first_programme() {
        let epg = sample_epg();
        let nn = now_next(&epg, "news.example", 50);
        assert_eq!(nn.now, None);
        assert_eq!(nn.next.unwrap().title, "A");
    }

    #[test]
    fn after_last_programme() {
        let epg = sample_epg();
        let nn = now_next(&epg, "news.example", 500);
        assert_eq!(nn, NowNext::default());
    }

    #[test]
    fn last_programme_has_no_next() {
        let epg = sample_epg();
        let nn = now_next(&epg, "news.example", 350);
        assert_eq!(nn.now.unwrap().title, "C");
        assert_eq!(nn.next, None);
    }

    #[test]
    fn gap_between_programmes() {
        let epg = Epg {
            channels: vec![],
            programmes: vec![prog("c", 0, 10, "A"), prog("c", 20, 30, "B")],
        };
        let nn = now_next(&epg, "c", 15);
        assert_eq!(nn.now, None);
        assert_eq!(nn.next.unwrap().title, "B");
    }

    #[test]
    fn ignores_other_channels() {
        let epg = sample_epg();
        let nn = now_next(&epg, "film.example", 160);
        assert_eq!(nn.now.unwrap().title, "F");
        assert_eq!(nn.next, None);
    }

    #[test]
    fn now_next_for_combines_matching_and_lookup() {
        let epg = sample_epg();
        let ch = playlist_channel("x", Some("news.example"));
        assert_eq!(now_next_for(&ch, &epg, 120).now.unwrap().title, "A");
        let unknown = playlist_channel("zzz", None);
        assert_eq!(now_next_for(&unknown, &epg, 120), NowNext::default());
    }
}
