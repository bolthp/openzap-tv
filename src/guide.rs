//! Linking playlist channels to EPG data: matching and "now / next" lookup.

use std::collections::HashMap;

use crate::epg::{Epg, Programme, normalize_name as norm};
use crate::m3u::{Channel, Playlist};

/// What is on air at a given moment and what follows.
///
/// Holds references into the `Epg` — no data is copied. The lifetime `'a`
/// ties this struct to the `Epg` it was taken from.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NowNext<'a> {
    pub now: Option<&'a Programme>,
    pub next: Option<&'a Programme>,
}

/// If programmes overlap, only this many earlier entries are checked for
/// one that is still running.
const LOOKBACK: usize = 16;

/// Precomputed lookup tables: built once per load, queried every frame.
///
/// Stores indices instead of references, so it does not borrow the
/// playlist or the EPG; the EPG is passed to each query instead.
#[derive(Debug, Default)]
pub struct GuideIndex {
    /// For each playlist channel (same order): id of its EPG channel.
    epg_ids: Vec<Option<String>>,
    /// EPG channel id -> indices into `epg.programmes`, sorted by start.
    by_channel: HashMap<String, Vec<usize>>,
}

impl GuideIndex {
    /// Matches playlist channels to EPG channels.
    ///
    /// Order: `tvg-id` == EPG id, then `tvg-name`, then the channel name
    /// against EPG display names. Comparisons are case-insensitive; when
    /// several EPG channels share an id or name, the first one wins.
    pub fn build(playlist: &Playlist, epg: &Epg) -> Self {
        let mut by_id: HashMap<String, &str> = HashMap::new();
        let mut by_name: HashMap<String, &str> = HashMap::new();
        for c in &epg.channels {
            by_id.entry(norm(&c.id)).or_insert(&c.id);
            by_name.entry(norm(&c.display_name)).or_insert(&c.id);
        }

        let epg_ids = playlist
            .channels
            .iter()
            .map(|ch| match_channel(ch, &by_id, &by_name).map(String::from))
            .collect();

        let mut by_channel: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in epg.programmes.iter().enumerate() {
            by_channel.entry(p.channel.clone()).or_default().push(i);
        }
        for list in by_channel.values_mut() {
            list.sort_by_key(|&i| epg.programmes[i].start);
        }
        GuideIndex {
            epg_ids,
            by_channel,
        }
    }

    /// EPG channel id matched to the playlist channel at `channel_idx`.
    pub fn epg_id(&self, channel_idx: usize) -> Option<&str> {
        self.epg_ids.get(channel_idx)?.as_deref()
    }

    /// Current and next programme of a playlist channel at Unix time `at`.
    ///
    /// `epg` must be the one the index was built from. If programmes
    /// overlap, the one that started last counts as "now".
    pub fn now_next<'a>(&self, epg: &'a Epg, channel_idx: usize, at: i64) -> NowNext<'a> {
        let Some(list) = self
            .epg_id(channel_idx)
            .and_then(|id| self.by_channel.get(id))
        else {
            return NowNext::default();
        };
        let prog = |&i: &usize| &epg.programmes[i];

        // Binary search: first entry that starts after `at`.
        let split = list.partition_point(|&i| epg.programmes[i].start <= at);
        let now = list[..split]
            .iter()
            .rev()
            .take(LOOKBACK)
            .map(prog)
            .find(|p| at < p.stop);
        // "Next" begins no earlier than the end of "now".
        let threshold = now.map_or(at, |p| p.stop.max(at));
        let next = list[split..]
            .iter()
            .map(prog)
            .find(|p| p.start >= threshold);
        NowNext { now, next }
    }
}

fn match_channel<'a>(
    channel: &Channel,
    by_id: &HashMap<String, &'a str>,
    by_name: &HashMap<String, &'a str>,
) -> Option<&'a str> {
    if let Some(found) = channel
        .tvg_id
        .as_deref()
        .and_then(|id| by_id.get(&norm(id)))
    {
        return Some(found);
    }
    [channel.tvg_name.as_deref(), Some(channel.name.as_str())]
        .into_iter()
        .flatten()
        .map(norm)
        .filter(|n| !n.is_empty())
        .find_map(|n| by_name.get(&n).copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::epg::EpgChannel;

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

    fn channel(name: &str, tvg_id: Option<&str>) -> Channel {
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
            // Deliberately not sorted: the index must sort by start.
            programmes: vec![
                prog("news.example", 300, 400, "C"),
                prog("news.example", 100, 200, "A"),
                prog("film.example", 150, 250, "F"),
                prog("news.example", 200, 300, "B"),
            ],
        }
    }

    /// Index over one playlist channel, as `(index, epg)`.
    fn single(ch: Channel, epg: Epg) -> (GuideIndex, Epg) {
        let playlist = Playlist { channels: vec![ch] };
        (GuideIndex::build(&playlist, &epg), epg)
    }

    fn title(p: Option<&Programme>) -> Option<&str> {
        p.map(|p| p.title.as_str())
    }

    #[test]
    fn matches_by_tvg_id_case_insensitive() {
        let (idx, _) = single(channel("Whatever", Some("NEWS.Example")), sample_epg());
        assert_eq!(idx.epg_id(0), Some("news.example"));
    }

    #[test]
    fn falls_back_to_name() {
        let (idx, _) = single(channel("  example film ", None), sample_epg());
        assert_eq!(idx.epg_id(0), Some("film.example"));
    }

    #[test]
    fn falls_back_to_name_when_tvg_id_unknown() {
        let (idx, _) = single(channel("Example News", Some("nope")), sample_epg());
        assert_eq!(idx.epg_id(0), Some("news.example"));
    }

    #[test]
    fn matches_non_ascii_names() {
        let epg = Epg {
            channels: vec![epg_channel("1", "Первый Канал")],
            programmes: vec![],
        };
        let (idx, _) = single(channel("первый канал", None), epg);
        assert_eq!(idx.epg_id(0), Some("1"));
    }

    #[test]
    fn no_match_gives_nothing() {
        let (idx, epg) = single(channel("Other", None), sample_epg());
        assert_eq!(idx.epg_id(0), None);
        assert_eq!(idx.now_next(&epg, 0, 150), NowNext::default());
        let (idx, _) = single(channel("", None), sample_epg());
        assert_eq!(idx.epg_id(0), None);
        assert_eq!(idx.epg_id(99), None); // out of range
    }

    #[test]
    fn first_epg_channel_wins_on_duplicate_names() {
        let epg = Epg {
            channels: vec![epg_channel("first", "Same"), epg_channel("second", "Same")],
            programmes: vec![],
        };
        let (idx, _) = single(channel("same", None), epg);
        assert_eq!(idx.epg_id(0), Some("first"));
    }

    fn news() -> (GuideIndex, Epg) {
        single(channel("x", Some("news.example")), sample_epg())
    }

    #[test]
    fn now_and_next_in_the_middle() {
        let (idx, epg) = news();
        let nn = idx.now_next(&epg, 0, 250);
        assert_eq!(title(nn.now), Some("B"));
        assert_eq!(title(nn.next), Some("C"));
    }

    #[test]
    fn stop_time_is_exclusive() {
        let (idx, epg) = news();
        assert_eq!(title(idx.now_next(&epg, 0, 200).now), Some("B"));
    }

    #[test]
    fn before_first_programme() {
        let (idx, epg) = news();
        let nn = idx.now_next(&epg, 0, 50);
        assert_eq!(nn.now, None);
        assert_eq!(title(nn.next), Some("A"));
    }

    #[test]
    fn after_last_programme() {
        let (idx, epg) = news();
        assert_eq!(idx.now_next(&epg, 0, 500), NowNext::default());
    }

    #[test]
    fn last_programme_has_no_next() {
        let (idx, epg) = news();
        let nn = idx.now_next(&epg, 0, 350);
        assert_eq!(title(nn.now), Some("C"));
        assert_eq!(nn.next, None);
    }

    #[test]
    fn gap_between_programmes() {
        let epg = Epg {
            channels: vec![epg_channel("c", "C")],
            programmes: vec![prog("c", 0, 10, "A"), prog("c", 20, 30, "B")],
        };
        let (idx, epg) = single(channel("C", None), epg);
        let nn = idx.now_next(&epg, 0, 15);
        assert_eq!(nn.now, None);
        assert_eq!(title(nn.next), Some("B"));
    }

    #[test]
    fn overlapping_programmes_latest_start_is_now() {
        let epg = Epg {
            channels: vec![epg_channel("c", "C")],
            programmes: vec![
                prog("c", 0, 100, "Long"),
                prog("c", 40, 60, "Short"),
                prog("c", 100, 200, "After"),
            ],
        };
        let (idx, epg) = single(channel("C", None), epg);
        let at_50 = idx.now_next(&epg, 0, 50);
        assert_eq!(title(at_50.now), Some("Short"));
        assert_eq!(title(at_50.next), Some("After"));
        // Short has ended but Long still runs.
        assert_eq!(title(idx.now_next(&epg, 0, 70).now), Some("Long"));
    }

    #[test]
    fn ignores_other_channels() {
        let (idx, epg) = single(channel("x", Some("film.example")), sample_epg());
        let nn = idx.now_next(&epg, 0, 160);
        assert_eq!(title(nn.now), Some("F"));
        assert_eq!(nn.next, None);
    }

    #[test]
    fn handles_many_programmes() {
        // 100k programmes on one channel: lookups must stay cheap.
        let programmes = (0..100_000)
            .map(|i| prog("c", i * 10, i * 10 + 10, "P"))
            .collect();
        let epg = Epg {
            channels: vec![epg_channel("c", "C")],
            programmes,
        };
        let (idx, epg) = single(channel("C", None), epg);
        let nn = idx.now_next(&epg, 0, 500_005);
        assert_eq!(nn.now.map(|p| p.start), Some(500_000));
        assert_eq!(nn.next.map(|p| p.start), Some(500_010));
    }
}
