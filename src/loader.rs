//! Loads and parses the playlist and (optionally) the EPG from their sources.

use std::fmt::Display;

use crate::epg::{self, ChannelFilter, Epg, EpgError, EpgFilter, normalize_name};
use crate::m3u::{self, Playlist};
use crate::source::{self, Source};

/// Guides are streamed, so this only guards against endless or absurd input.
const MAX_EPG_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Programmes kept around "now": a little history for long shows already
/// running, and a few days ahead.
const EPG_KEEP_PAST_SECS: i64 = 3 * 3600;
const EPG_KEEP_FUTURE_SECS: i64 = 4 * 24 * 3600;

/// Result of loading both sources. Errors are user-facing messages that
/// never contain URLs.
pub struct Loaded {
    pub playlist: Result<Playlist, String>,
    /// `None` when no EPG source was given.
    pub epg: Option<Result<Epg, String>>,
}

/// Blocking: call it from a background thread. `now` (Unix seconds) sets
/// the window of programmes kept from the guide.
pub fn load_sources(playlist: &str, epg_src: &str, now: i64) -> Loaded {
    let playlist = if playlist.trim().is_empty() {
        Err("no playlist source given".to_string())
    } else {
        fetch_and_parse(playlist, m3u::parse)
    };
    let epg = if epg_src.trim().is_empty() {
        None
    } else {
        let filter = EpgFilter {
            window: Some((now - EPG_KEEP_PAST_SECS, now + EPG_KEEP_FUTURE_SECS)),
            // Without a playlist there is nothing to match against: keep all channels.
            channels: playlist.as_ref().ok().map(channel_filter),
        };
        Some(load_epg(epg_src, &filter))
    };
    Loaded { playlist, epg }
}

/// The EPG channels a playlist can match (same rules as `GuideIndex`).
fn channel_filter(playlist: &Playlist) -> ChannelFilter {
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

fn load_epg(input: &str, filter: &EpgFilter) -> Result<Epg, String> {
    let source = Source::parse(input);
    let opened = source::open(&source, MAX_EPG_BYTES).map_err(|e| e.to_string())?;
    let compressed = opened.compressed;
    epg::parse_reader(opened.reader, filter).map_err(|e| match e {
        EpgError::Io(kind) => source::read_error(&source, kind, compressed).to_string(),
        other => other.to_string(),
    })
}

fn fetch_and_parse<T, E: Display>(
    input: &str,
    parse: impl FnOnce(&str) -> Result<T, E>,
) -> Result<T, String> {
    let text = source::fetch_text(&Source::parse(input)).map_err(|e| e.to_string())?;
    parse(&text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_file(name: &str, data: &str) -> String {
        let dir = std::env::temp_dir().join(format!("openzap-loader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path: PathBuf = dir.join(name);
        std::fs::write(&path, data).unwrap();
        path.to_string_lossy().into_owned()
    }

    const PLAYLIST: &str = "#EXTM3U\n#EXTINF:-1 tvg-id=\"a\",A\nhttp://example.com/a\n";
    const EPG: &str = "<tv><channel id=\"a\"><display-name>A</display-name></channel></tv>";

    #[test]
    fn loads_playlist_without_epg() {
        let loaded = load_sources(&temp_file("a.m3u", PLAYLIST), "  ", 0);
        assert_eq!(loaded.playlist.unwrap().channels.len(), 1);
        assert!(loaded.epg.is_none());
    }

    #[test]
    fn loads_playlist_and_epg() {
        let loaded = load_sources(&temp_file("b.m3u", PLAYLIST), &temp_file("b.xml", EPG), 0);
        assert!(loaded.playlist.is_ok());
        assert_eq!(loaded.epg.unwrap().unwrap().channels.len(), 1);
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
        let loaded = load_sources(
            &temp_file("f.m3u", PLAYLIST),
            &temp_file("f.xml", epg_xml),
            now,
        );
        let epg = loaded.epg.unwrap().unwrap();
        assert_eq!(epg.channels.len(), 1);
        let titles: Vec<_> = epg.programmes.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Now"]);
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
        assert!(load_sources("", "", 0).playlist.is_err());
    }

    #[test]
    fn invalid_playlist_reports_parse_error() {
        let loaded = load_sources(&temp_file("bad.m3u", "not a playlist"), "", 0);
        assert_eq!(loaded.playlist.unwrap_err(), "missing #EXTM3U header");
    }

    #[test]
    fn broken_epg_does_not_break_playlist() {
        let loaded = load_sources(
            &temp_file("c.m3u", PLAYLIST),
            &temp_file("bad.xml", "<tv><a></tv>"),
            0,
        );
        assert!(loaded.playlist.is_ok());
        assert!(loaded.epg.unwrap().is_err());
    }

    #[test]
    fn url_errors_do_not_leak_the_url() {
        // Port 1 is not listening: connection is refused immediately.
        let loaded = load_sources("http://127.0.0.1:1/list.m3u?token=fakekey123", "", 0);
        let msg = loaded.playlist.unwrap_err();
        assert!(!msg.contains("fakekey123"), "leaked: {msg}");
    }
}
