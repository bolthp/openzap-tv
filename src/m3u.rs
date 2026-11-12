//! Parser for extended M3U (`#EXTM3U`) playlists.

use std::fmt;

use crate::catchup::{self, Catchup};
use crate::mask::mask_url;

/// A single playlist entry.
#[derive(Clone, PartialEq, Default)]
pub struct Channel {
    pub name: String,
    pub url: String,
    pub tvg_id: Option<String>,
    pub tvg_name: Option<String>,
    pub tvg_logo: Option<String>,
    pub group_title: Option<String>,
    /// Channel number from `tvg-chno`, if the playlist assigns one.
    pub tvg_chno: Option<u32>,
    /// Archive of past programmes, if the playlist offers one.
    pub catchup: Option<Catchup>,
}

impl Channel {
    /// Stable identifier for favorites: `tvg-id` if present, else the name.
    /// Returns a borrowed `&str`, so no allocation.
    pub fn key(&self) -> &str {
        self.tvg_id.as_deref().unwrap_or(&self.name)
    }
}

// Manual `Debug` (a trait) so that `{:?}` never prints a URL with secrets.
impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("name", &self.name)
            .field("url", &mask_url(&self.url))
            .field("tvg_id", &self.tvg_id)
            .field("group_title", &self.group_title)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Playlist {
    pub channels: Vec<Channel>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The first non-empty line is not `#EXTM3U`.
    MissingHeader,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::MissingHeader => write!(f, "missing #EXTM3U header"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parses playlist text. Lenient: unknown `#` lines are ignored.
pub fn parse(input: &str) -> Result<Playlist, ParseError> {
    let mut lines = input
        .trim_start_matches('\u{feff}') // optional UTF-8 BOM
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty());

    let Some(header) = lines.next().and_then(|l| l.strip_prefix("#EXTM3U")) else {
        return Err(ParseError::MissingHeader);
    };
    // Archive attributes in the header apply to every channel.
    let mut defaults = catchup::Attrs::default();
    for (key, value) in parse_attrs(header) {
        defaults.set(&key.to_ascii_lowercase(), value);
    }

    let mut channels = Vec::new();
    let mut pending: Option<(Channel, catchup::Attrs)> = None;

    for line in lines {
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            pending = Some(parse_extinf(info));
        } else if let Some(group) = line.strip_prefix("#EXTGRP:") {
            if let Some((ch, _)) = pending.as_mut() {
                ch.group_title
                    .get_or_insert_with(|| group.trim().to_string());
            }
        } else if line.starts_with('#') {
            // Other directives (#EXTVLCOPT, comments, ...) are ignored.
        } else {
            let (mut ch, attrs) = pending.take().unwrap_or_default();
            if ch.name.is_empty() {
                ch.name = line.to_string();
            }
            ch.url = line.to_string();
            ch.catchup = attrs.or(&defaults).resolve(line);
            channels.push(ch);
        }
    }
    Ok(Playlist { channels })
}

/// Parses the part after `#EXTINF:` — `-1 tvg-id="x" group-title="y",Display Name`.
/// The archive attributes are returned apart: they need the URL.
fn parse_extinf(info: &str) -> (Channel, catchup::Attrs) {
    let (head, name) = split_at_name_comma(info);
    let mut ch = Channel {
        name: name.to_string(),
        ..Channel::default()
    };
    let mut archive = catchup::Attrs::default();
    for (key, value) in parse_attrs(head) {
        let key = key.to_ascii_lowercase();
        if archive.set(&key, value) {
            continue;
        }
        if key == "tvg-chno" {
            ch.tvg_chno = value.trim().parse().ok().filter(|&n| n > 0);
            continue;
        }
        let slot = match key.as_str() {
            "tvg-id" => &mut ch.tvg_id,
            "tvg-name" => &mut ch.tvg_name,
            "tvg-logo" => &mut ch.tvg_logo,
            "group-title" => &mut ch.group_title,
            _ => continue,
        };
        *slot = Some(value.to_string()).filter(|v| !v.is_empty());
    }
    (ch, archive)
}

/// Splits at the first comma that is not inside double quotes.
fn split_at_name_comma(s: &str) -> (&str, &str) {
    let mut in_quotes = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => return (&s[..i], s[i + 1..].trim()),
            _ => {}
        }
    }
    (s, "")
}

/// Extracts `key="value"` / `key=value` pairs.
///
/// The returned slices borrow from `s` (Rust infers the lifetime), so they
/// cannot outlive it — no copying needed.
fn parse_attrs(s: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].split_whitespace().next_back().unwrap_or("");
        let after = &rest[eq + 1..];
        let (value, remaining) = if let Some(quoted) = after.strip_prefix('"') {
            match quoted.find('"') {
                Some(end) => (&quoted[..end], &quoted[end + 1..]),
                None => (quoted, ""),
            }
        } else {
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        if !key.is_empty() {
            out.push((key, value));
        }
        rest = remaining;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_archive_with_header_defaults() {
        let pl = parse(
            "#EXTM3U url-tvg=\"http://example.com/epg.xml\" catchup=\"shift\" catchup-days=\"3\"\n\
             #EXTINF:-1,A\nhttp://example.com/a\n\
             #EXTINF:-1 tvg-rec=\"7\",B\nhttp://example.com/b\n\
             #EXTINF:-1 catchup=\"flussonic\",C\nhttp://example.com/c/index.m3u8\n",
        )
        .unwrap();
        let archive: Vec<_> = pl
            .channels
            .iter()
            .map(|c| c.catchup.as_ref().map(|a| (a.mode, a.days)))
            .collect();
        assert_eq!(
            archive,
            [
                Some((catchup::Mode::Shift, 3)),
                Some((catchup::Mode::Shift, 7)),
                Some((catchup::Mode::Flussonic, 3)),
            ]
        );
        let plain = parse("#EXTM3U\n#EXTINF:-1,A\nhttp://example.com/a\n").unwrap();
        assert_eq!(plain.channels[0].catchup, None);
    }

    #[test]
    fn parses_channel_numbers() {
        let pl = parse(
            "#EXTM3U\n\
             #EXTINF:-1 tvg-chno=\"12\",A\nhttp://example.com/a\n\
             #EXTINF:-1 tvg-chno=\"x\",B\nhttp://example.com/b\n\
             #EXTINF:-1 TVG-CHNO=\"0\",C\nhttp://example.com/c\n",
        )
        .unwrap();
        let numbers: Vec<_> = pl.channels.iter().map(|c| c.tvg_chno).collect();
        assert_eq!(numbers, [Some(12), None, None]);
    }

    #[test]
    fn parses_full_entry() {
        let text = "#EXTM3U\n\
            #EXTINF:-1 tvg-id=\"news.example\" tvg-name=\"News\" tvg-logo=\"http://example.com/l.png\" group-title=\"News\",Example News\n\
            http://example.com/news.m3u8\n";
        let pl = parse(text).unwrap();
        assert_eq!(pl.channels.len(), 1);
        let ch = &pl.channels[0];
        assert_eq!(ch.name, "Example News");
        assert_eq!(ch.url, "http://example.com/news.m3u8");
        assert_eq!(ch.tvg_id.as_deref(), Some("news.example"));
        assert_eq!(ch.tvg_name.as_deref(), Some("News"));
        assert_eq!(ch.tvg_logo.as_deref(), Some("http://example.com/l.png"));
        assert_eq!(ch.group_title.as_deref(), Some("News"));
    }

    #[test]
    fn missing_header_is_error() {
        assert_eq!(
            parse("http://example.com/a"),
            Err(ParseError::MissingHeader)
        );
        assert_eq!(parse(""), Err(ParseError::MissingHeader));
    }

    #[test]
    fn handles_bom_crlf_and_blank_lines() {
        let text = "\u{feff}#EXTM3U\r\n\r\n#EXTINF:-1,One\r\nhttp://example.com/1\r\n\r\n";
        let pl = parse(text).unwrap();
        assert_eq!(pl.channels.len(), 1);
        assert_eq!(pl.channels[0].name, "One");
        assert_eq!(pl.channels[0].url, "http://example.com/1");
    }

    #[test]
    fn comma_inside_quotes_is_not_name_separator() {
        let text = "#EXTM3U\n#EXTINF:-1 group-title=\"Movies, Drama\",Film\nhttp://example.com/f\n";
        let ch = &parse(text).unwrap().channels[0];
        assert_eq!(ch.group_title.as_deref(), Some("Movies, Drama"));
        assert_eq!(ch.name, "Film");
    }

    #[test]
    fn url_without_extinf_uses_url_as_name() {
        let pl = parse("#EXTM3U\nhttp://example.com/raw\n").unwrap();
        assert_eq!(pl.channels[0].name, "http://example.com/raw");
    }

    #[test]
    fn ignores_unknown_directives_and_applies_extgrp() {
        let text = "#EXTM3U\n#EXTINF:-1,Ch\n#EXTVLCOPT:http-user-agent=x\n#EXTGRP:Sports\nhttp://example.com/c\n";
        let ch = &parse(text).unwrap().channels[0];
        assert_eq!(ch.group_title.as_deref(), Some("Sports"));
    }

    #[test]
    fn explicit_group_title_wins_over_extgrp() {
        let text = "#EXTM3U\n#EXTINF:-1 group-title=\"A\",Ch\n#EXTGRP:B\nhttp://example.com/c\n";
        let ch = &parse(text).unwrap().channels[0];
        assert_eq!(ch.group_title.as_deref(), Some("A"));
    }

    #[test]
    fn empty_attribute_becomes_none() {
        let text = "#EXTM3U\n#EXTINF:-1 tvg-id=\"\",Ch\nhttp://example.com/c\n";
        assert_eq!(parse(text).unwrap().channels[0].tvg_id, None);
    }

    #[test]
    fn key_prefers_tvg_id_over_name() {
        let with_id = Channel {
            name: "N".into(),
            tvg_id: Some("id.example".into()),
            ..Channel::default()
        };
        let without = Channel {
            name: "N".into(),
            ..Channel::default()
        };
        assert_eq!(with_id.key(), "id.example");
        assert_eq!(without.key(), "N");
    }

    #[test]
    fn debug_output_masks_secrets() {
        let text = "#EXTM3U\n#EXTINF:-1,Ch\nhttp://example.com/c?token=fakekey123\n";
        let pl = parse(text).unwrap();
        let dbg = format!("{:?}", pl);
        assert!(!dbg.contains("fakekey123"));
        assert!(dbg.contains("token=***"));
    }
}
