//! Parser for XMLTV programme guides.

use std::collections::HashSet;
use std::fmt;
use std::io::{BufRead, ErrorKind};

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpgChannel {
    pub id: String,
    /// All `display-name`s in file order, without duplicates; the first is
    /// the main one. Guides often list variants ("Arena Sport 1 HD RS",
    /// "Arena Sport 1 RS"), and playlists use any of them.
    pub display_names: Vec<String>,
    pub icon: Option<String>,
}

impl EpgChannel {
    /// The main display name ("" if there is none).
    pub fn display_name(&self) -> &str {
        self.display_names.first().map_or("", String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Programme {
    /// Id of the `EpgChannel` this programme belongs to.
    pub channel: String,
    /// Unix timestamps (seconds, UTC).
    pub start: i64,
    pub stop: i64,
    pub title: String,
    pub desc: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Epg {
    pub channels: Vec<EpgChannel>,
    pub programmes: Vec<Programme>,
}

impl Epg {
    /// Programmes of one channel. The returned iterator borrows both `self`
    /// and `id`, hence the shared lifetime `'a`.
    pub fn programmes_for<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Programme> + 'a {
        self.programmes.iter().filter(move |p| p.channel == id)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EpgError {
    /// The XML is malformed.
    Xml(String),
    /// Reading the input failed (network, file, decompression, size limit).
    /// Only the kind is kept: messages could contain a URL.
    Io(ErrorKind),
}

impl fmt::Display for EpgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EpgError::Xml(msg) => write!(f, "invalid XMLTV: {msg}"),
            EpgError::Io(kind) => write!(f, "read error: {kind}"),
        }
    }
}

impl std::error::Error for EpgError {}

/// Text elements we collect.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    DisplayName,
    Title,
    Desc,
    Category,
}

#[derive(Default)]
struct ProgrammeBuilder {
    channel: String,
    start: Option<i64>,
    stop: Option<i64>,
    title: Option<String>,
    desc: Option<String>,
    category: Option<String>,
}

impl ProgrammeBuilder {
    /// Programmes without a channel, valid times or title are dropped.
    fn build(self) -> Option<Programme> {
        let (start, stop) = (self.start?, self.stop?);
        if self.channel.is_empty() || stop < start {
            return None;
        }
        Some(Programme {
            channel: self.channel,
            start,
            stop,
            title: self.title?,
            desc: self.desc,
            category: self.category,
        })
    }
}

/// Normalizes a channel id or name for matching: lowercase, runs of
/// whitespace collapsed to one space, "ё" read as "е".
pub fn normalize_name(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .replace('ё', "е")
}

/// Which EPG channels to keep, by normalized id or display name
/// (see [`normalize_name`]).
#[derive(Debug, Default, Clone)]
pub struct ChannelFilter {
    pub ids: HashSet<String>,
    pub names: HashSet<String>,
}

impl ChannelFilter {
    fn wants(&self, channel: &EpgChannel) -> bool {
        self.ids.contains(&normalize_name(&channel.id))
            || channel
                .display_names
                .iter()
                .any(|n| self.names.contains(&normalize_name(n)))
    }
}

/// What to keep while parsing. The default keeps everything.
#[derive(Debug, Default, Clone)]
pub struct EpgFilter {
    /// Keep only programmes overlapping `(from, to)` (Unix seconds).
    pub window: Option<(i64, i64)>,
    /// Keep only these channels and their programmes.
    pub channels: Option<ChannelFilter>,
}

/// Parses XMLTV text. Lenient: incomplete channels/programmes are skipped,
/// only malformed XML is an error.
pub fn parse(xml: &str) -> Result<Epg, EpgError> {
    parse_reader(xml.as_bytes(), &EpgFilter::default())
}

/// Parses XMLTV from a stream, keeping only what `filter` asks for. Memory
/// use is proportional to what is kept, not to the size of the input.
///
/// Channel filtering relies on `<channel>` elements coming before the
/// programmes that refer to them (as XMLTV requires); a programme whose
/// channel id matches a wanted id directly is kept regardless.
pub fn parse_reader(input: impl BufRead, filter: &EpgFilter) -> Result<Epg, EpgError> {
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();
    let mut epg = Epg::default();
    let mut channel: Option<EpgChannel> = None;
    let mut programme: Option<ProgrammeBuilder> = None;
    let mut field: Option<Field> = None;
    let mut text = String::new();
    // Normalized ids of the channels kept so far.
    let mut kept_ids: HashSet<String> = HashSet::new();

    let wanted_programme = |p: &ProgrammeBuilder, kept_ids: &HashSet<String>| {
        let in_window = match (filter.window, p.start, p.stop) {
            (Some((from, to)), Some(start), Some(stop)) => stop > from && start < to,
            (Some(_), _, _) => false, // no valid times: useless anyway
            (None, _, _) => true,
        };
        in_window
            && filter.channels.as_ref().is_none_or(|f| {
                let id = normalize_name(&p.channel);
                kept_ids.contains(&id) || f.ids.contains(&id)
            })
    };

    loop {
        buf.clear();
        let event = reader.read_event_into(&mut buf).map_err(|e| match e {
            quick_xml::Error::Io(io) => EpgError::Io(io.kind()),
            other => EpgError::Xml(other.to_string()),
        })?;
        match event {
            Event::Start(e) => match e.local_name().as_ref() {
                "channel" => {
                    channel = Some(EpgChannel {
                        id: attr(&e, "id").unwrap_or_default(),
                        display_names: Vec::new(),
                        icon: None,
                    });
                }
                "programme" => {
                    let candidate = ProgrammeBuilder {
                        channel: attr(&e, "channel").unwrap_or_default(),
                        start: attr(&e, "start").as_deref().and_then(parse_time),
                        stop: attr(&e, "stop").as_deref().and_then(parse_time),
                        ..ProgrammeBuilder::default()
                    };
                    // Unwanted programmes are dropped right away; their child
                    // elements are then ignored because `programme` is None.
                    programme = wanted_programme(&candidate, &kept_ids).then_some(candidate);
                }
                "display-name" if channel.is_some() => {
                    start_field(&mut field, &mut text, Field::DisplayName)
                }
                "title" if programme.is_some() => start_field(&mut field, &mut text, Field::Title),
                "desc" if programme.is_some() => start_field(&mut field, &mut text, Field::Desc),
                "category" if programme.is_some() => {
                    start_field(&mut field, &mut text, Field::Category)
                }
                "icon" => set_icon(&mut channel, &e),
                _ => {}
            },
            Event::Empty(e) if e.local_name().as_ref() == "icon" => set_icon(&mut channel, &e),
            Event::Text(t) if field.is_some() => text.push_str(&t.xml10_content()),
            Event::CData(t) if field.is_some() => {
                text.push_str(&t.into_inner());
            }
            Event::GeneralRef(r) if field.is_some() => {
                if let Some(c) = resolve_ref(&r) {
                    text.push(c);
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "display-name" | "title" | "desc" | "category" => {
                    let value = text.trim().to_string();
                    if !value.is_empty() {
                        store_field(field, value, &mut channel, &mut programme);
                    }
                    field = None;
                }
                "channel" => {
                    let keep = |c: &EpgChannel| {
                        !c.id.is_empty() && filter.channels.as_ref().is_none_or(|f| f.wants(c))
                    };
                    if let Some(ch) = channel.take().filter(keep) {
                        kept_ids.insert(normalize_name(&ch.id));
                        epg.channels.push(ch);
                    }
                }
                "programme" => {
                    if let Some(p) = programme.take().and_then(ProgrammeBuilder::build) {
                        epg.programmes.push(p);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(epg)
}

fn start_field(field: &mut Option<Field>, text: &mut String, which: Field) {
    *field = Some(which);
    text.clear();
}

/// Stores the first non-empty value of each programme field; later
/// duplicates (e.g. translated titles) are ignored. Channels keep every
/// display name.
fn store_field(
    field: Option<Field>,
    value: String,
    channel: &mut Option<EpgChannel>,
    programme: &mut Option<ProgrammeBuilder>,
) {
    match (field, channel.as_mut(), programme.as_mut()) {
        (Some(Field::DisplayName), Some(ch), _) => {
            if !ch.display_names.contains(&value) {
                ch.display_names.push(value);
            }
        }
        (Some(Field::Title), _, Some(p)) => {
            p.title.get_or_insert(value);
        }
        (Some(Field::Desc), _, Some(p)) => {
            p.desc.get_or_insert(value);
        }
        (Some(Field::Category), _, Some(p)) => {
            p.category.get_or_insert(value);
        }
        _ => {}
    }
}

fn set_icon(channel: &mut Option<EpgChannel>, e: &BytesStart) {
    if let (Some(ch), Some(src)) = (channel.as_mut(), attr(e, "src")) {
        ch.icon.get_or_insert(src);
    }
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten() // skip malformed attributes
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| a.normalized_value(XmlVersion::Implicit1_0).ok())
        .map(|v| v.into_owned())
}

/// Resolves `&amp;`-style and `&#233;`-style references.
fn resolve_ref(r: &quick_xml::events::BytesRef) -> Option<char> {
    if r.is_char_ref() {
        return r.resolve_char_ref().ok().flatten();
    }
    match &**r {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    }
}

/// Parses an XMLTV timestamp (`YYYYMMDDHHMMSS [+-HHMM]`) into Unix seconds.
/// Without an offset UTC is assumed; seconds may be omitted.
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let (datetime, offset) = match s.split_once(char::is_whitespace) {
        Some((d, o)) => (d, Some(o.trim())),
        None => (s, None),
    };
    if !datetime.bytes().all(|b| b.is_ascii_digit()) || !matches!(datetime.len(), 12 | 14) {
        return None;
    }
    let num = |range: std::ops::Range<usize>| datetime.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (num(0..4)?, num(4..6)?, num(6..8)?);
    let (hour, min) = (num(8..10)?, num(10..12)?);
    let sec = if datetime.len() == 14 {
        num(12..14)?
    } else {
        0
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }

    let offset_secs = match offset {
        None | Some("") => 0,
        Some(o) => parse_offset(o)?,
    };
    let days = crate::clock::days_from_civil(year, month as u32, day as u32);
    Some(days * 86_400 + hour * 3600 + min * 60 + sec - offset_secs)
}

/// `+0200` / `-0530` -> seconds east of UTC.
fn parse_offset(o: &str) -> Option<i64> {
    let sign = match o.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = &o[1..];
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let mins: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3600 + mins * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<tv generator-info-name="test">
  <channel id="news.example">
    <display-name>Example News</display-name>
    <display-name>News HD</display-name>
    <icon src="http://example.com/news.png"/>
  </channel>
  <channel id="film.example"><display-name>Example Film</display-name></channel>
  <programme start="20240229120000 +0000" stop="20240229130000 +0000" channel="news.example">
    <title lang="en">Noon &amp; News</title>
    <desc>Caf&#233; report</desc>
    <category>News</category>
  </programme>
  <programme start="20240229130000 +0200" stop="20240229140000 +0200" channel="film.example">
    <title>Movie</title>
  </programme>
</tv>"#;

    #[test]
    fn parses_channels() {
        let epg = parse(SAMPLE).unwrap();
        assert_eq!(epg.channels.len(), 2);
        assert_eq!(epg.channels[0].id, "news.example");
        assert_eq!(epg.channels[0].display_name(), "Example News");
        assert_eq!(epg.channels[0].display_names, ["Example News", "News HD"]);
        assert_eq!(
            epg.channels[0].icon.as_deref(),
            Some("http://example.com/news.png")
        );
        assert_eq!(epg.channels[1].icon, None);
    }

    #[test]
    fn duplicate_display_names_are_kept_once() {
        let xml = r#"<tv><channel id="a"><display-name>A</display-name>
            <display-name>A</display-name><display-name>A HD</display-name></channel></tv>"#;
        assert_eq!(parse(xml).unwrap().channels[0].display_names, ["A", "A HD"]);
        let empty = EpgChannel {
            id: "x".into(),
            display_names: vec![],
            icon: None,
        };
        assert_eq!(empty.display_name(), "");
    }

    #[test]
    fn filter_matches_any_display_name() {
        let filter = EpgFilter {
            window: None,
            channels: Some(channels(&[], &["news hd"])),
        };
        let epg = parse_reader(SAMPLE.as_bytes(), &filter).unwrap();
        assert_eq!(epg.channels.len(), 1);
        assert_eq!(epg.channels[0].id, "news.example");
        assert_eq!(epg.programmes.len(), 1, "its programmes are kept too");
    }

    #[test]
    fn names_normalize_case_spaces_and_yo() {
        assert_eq!(normalize_name("  Ёлки  ТВ\tHD "), "елки тв hd");
        assert_eq!(
            normalize_name("Первый Канал"),
            normalize_name("первый канал")
        );
    }

    #[test]
    fn parses_programmes_with_entities() {
        let epg = parse(SAMPLE).unwrap();
        assert_eq!(epg.programmes.len(), 2);
        let p = &epg.programmes[0];
        assert_eq!(p.title, "Noon & News");
        assert_eq!(p.desc.as_deref(), Some("Café report"));
        assert_eq!(p.category.as_deref(), Some("News"));
        assert_eq!(p.start, 1_709_208_000);
        assert_eq!(p.stop, 1_709_211_600);
    }

    #[test]
    fn applies_timezone_offset() {
        let epg = parse(SAMPLE).unwrap();
        // 13:00 +0200 == 11:00 UTC
        assert_eq!(epg.programmes[1].start, 1_709_204_400);
    }

    #[test]
    fn programmes_for_filters_by_channel() {
        let epg = parse(SAMPLE).unwrap();
        let titles: Vec<_> = epg
            .programmes_for("film.example")
            .map(|p| &p.title)
            .collect();
        assert_eq!(titles, ["Movie"]);
        assert_eq!(epg.programmes_for("unknown").count(), 0);
    }

    #[test]
    fn skips_incomplete_programmes() {
        let xml = r#"<tv>
            <programme start="bad" stop="20240229130000" channel="a"><title>X</title></programme>
            <programme start="20240229120000" stop="20240229130000" channel="a"></programme>
            <programme start="20240229140000" stop="20240229130000" channel="a"><title>Backwards</title></programme>
            <programme start="20240229120000" stop="20240229130000" channel="a"><title>Ok</title></programme>
        </tv>"#;
        let epg = parse(xml).unwrap();
        assert_eq!(epg.programmes.len(), 1);
        assert_eq!(epg.programmes[0].title, "Ok");
    }

    #[test]
    fn malformed_xml_is_error() {
        assert!(matches!(
            parse("<tv><channel id=\"a\"></tv>"),
            Err(EpgError::Xml(_))
        ));
    }

    #[test]
    fn empty_tv_is_ok() {
        assert_eq!(parse("<tv></tv>").unwrap(), Epg::default());
    }

    fn filtered(xml: &str, filter: &EpgFilter) -> Epg {
        parse_reader(xml.as_bytes(), filter).unwrap()
    }

    #[test]
    fn window_keeps_only_overlapping_programmes() {
        // 12:00-13:00 and 13:00-14:00 UTC on 2024-02-29.
        let xml = r#"<tv>
            <programme start="20240229120000" stop="20240229130000" channel="a"><title>Noon</title></programme>
            <programme start="20240229130000" stop="20240229140000" channel="a"><title>One</title></programme>
        </tv>"#;
        let at_1230 = 1_709_209_800;
        let filter = EpgFilter {
            window: Some((at_1230, at_1230 + 60)),
            channels: None,
        };
        let titles: Vec<_> = filtered(xml, &filter)
            .programmes
            .into_iter()
            .map(|p| p.title)
            .collect();
        assert_eq!(titles, ["Noon"]);
    }

    fn channels(ids: &[&str], names: &[&str]) -> ChannelFilter {
        ChannelFilter {
            ids: ids.iter().map(|s| normalize_name(s)).collect(),
            names: names.iter().map(|s| normalize_name(s)).collect(),
        }
    }

    #[test]
    fn channel_filter_by_id_or_display_name() {
        let filter = EpgFilter {
            window: None,
            channels: Some(channels(&["NEWS.EXAMPLE"], &["example film"])),
        };
        let epg = filtered(SAMPLE, &filter);
        assert_eq!(epg.channels.len(), 2); // one by id, one by name
        assert_eq!(epg.programmes.len(), 2);

        let only_news = EpgFilter {
            window: None,
            channels: Some(channels(&["news.example"], &[])),
        };
        let epg = filtered(SAMPLE, &only_news);
        assert_eq!(epg.channels.len(), 1);
        assert_eq!(epg.programmes.len(), 1);
        assert_eq!(epg.programmes[0].channel, "news.example");
    }

    #[test]
    fn programme_with_wanted_id_is_kept_without_channel_element() {
        let xml = r#"<tv><programme start="20240229120000" stop="20240229130000" channel="x"><title>T</title></programme></tv>"#;
        let filter = EpgFilter {
            window: None,
            channels: Some(channels(&["x"], &[])),
        };
        assert_eq!(filtered(xml, &filter).programmes.len(), 1);
    }

    #[test]
    fn read_errors_keep_only_the_kind() {
        struct Failing;
        impl std::io::Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    "secret http://example.com/?token=x",
                ))
            }
        }
        let err =
            parse_reader(std::io::BufReader::new(Failing), &EpgFilter::default()).unwrap_err();
        assert_eq!(err, EpgError::Io(ErrorKind::TimedOut));
        assert!(!err.to_string().contains("token"));
    }

    #[test]
    fn time_parsing() {
        assert_eq!(parse_time("19700101000000"), Some(0));
        assert_eq!(parse_time("20000101000000 +0000"), Some(946_684_800));
        assert_eq!(parse_time("20240229120000"), Some(1_709_208_000));
        assert_eq!(parse_time("202402291200"), Some(1_709_208_000));
        assert_eq!(
            parse_time("20240229120000 -0530"),
            Some(1_709_208_000 + 19_800)
        );
    }

    #[test]
    fn time_parsing_rejects_garbage() {
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("2024"), None);
        assert_eq!(parse_time("20241301000000"), None); // month 13
        assert_eq!(parse_time("20240229120000 +02"), None);
        assert_eq!(parse_time("2024022912000x"), None);
    }
}
