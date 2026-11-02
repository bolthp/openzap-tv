//! Loads and parses the playlist and (optionally) the EPG from their sources.

use std::fmt::Display;

use crate::epg::{self, Epg};
use crate::m3u::{self, Playlist};
use crate::source::{self, Source};

/// Result of loading both sources. Errors are user-facing messages that
/// never contain URLs.
pub struct Loaded {
    pub playlist: Result<Playlist, String>,
    /// `None` when no EPG source was given.
    pub epg: Option<Result<Epg, String>>,
}

/// Blocking: call it from a background thread.
pub fn load_sources(playlist: &str, epg_src: &str) -> Loaded {
    let playlist = if playlist.trim().is_empty() {
        Err("no playlist source given".to_string())
    } else {
        fetch_and_parse(playlist, m3u::parse)
    };
    let epg = if epg_src.trim().is_empty() {
        None
    } else {
        Some(fetch_and_parse(epg_src, epg::parse))
    };
    Loaded { playlist, epg }
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
        let loaded = load_sources(&temp_file("a.m3u", PLAYLIST), "  ");
        assert_eq!(loaded.playlist.unwrap().channels.len(), 1);
        assert!(loaded.epg.is_none());
    }

    #[test]
    fn loads_playlist_and_epg() {
        let loaded = load_sources(&temp_file("b.m3u", PLAYLIST), &temp_file("b.xml", EPG));
        assert!(loaded.playlist.is_ok());
        assert_eq!(loaded.epg.unwrap().unwrap().channels.len(), 1);
    }

    #[test]
    fn empty_playlist_source_is_an_error() {
        assert!(load_sources("", "").playlist.is_err());
    }

    #[test]
    fn invalid_playlist_reports_parse_error() {
        let loaded = load_sources(&temp_file("bad.m3u", "not a playlist"), "");
        assert_eq!(loaded.playlist.unwrap_err(), "missing #EXTM3U header");
    }

    #[test]
    fn broken_epg_does_not_break_playlist() {
        let loaded = load_sources(
            &temp_file("c.m3u", PLAYLIST),
            &temp_file("bad.xml", "<tv><a></tv>"),
        );
        assert!(loaded.playlist.is_ok());
        assert!(loaded.epg.unwrap().is_err());
    }

    #[test]
    fn url_errors_do_not_leak_the_url() {
        // Port 1 is not listening: connection is refused immediately.
        let loaded = load_sources("http://127.0.0.1:1/list.m3u?token=fakekey123", "");
        let msg = loaded.playlist.unwrap_err();
        assert!(!msg.contains("fakekey123"), "leaked: {msg}");
    }
}
