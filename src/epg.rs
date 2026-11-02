//! Parser for XMLTV programme guides.

use std::fmt;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpgChannel {
    pub id: String,
    pub display_name: String,
    pub icon: Option<String>,
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
}

impl fmt::Display for EpgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EpgError::Xml(msg) => write!(f, "invalid XMLTV: {msg}"),
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

/// Parses XMLTV text. Lenient: incomplete channels/programmes are skipped,
/// only malformed XML is an error.
pub fn parse(xml: &str) -> Result<Epg, EpgError> {
    let mut reader = Reader::from_str(xml);
    let mut epg = Epg::default();
    let mut channel: Option<EpgChannel> = None;
    let mut programme: Option<ProgrammeBuilder> = None;
    let mut field: Option<Field> = None;
    let mut text = String::new();

    loop {
        let event = reader
            .read_event()
            .map_err(|e| EpgError::Xml(e.to_string()))?;
        match event {
            Event::Start(e) => match e.local_name().as_ref() {
                "channel" => {
                    channel = Some(EpgChannel {
                        id: attr(&e, "id").unwrap_or_default(),
                        display_name: String::new(),
                        icon: None,
                    });
                }
                "programme" => {
                    programme = Some(ProgrammeBuilder {
                        channel: attr(&e, "channel").unwrap_or_default(),
                        start: attr(&e, "start").as_deref().and_then(parse_time),
                        stop: attr(&e, "stop").as_deref().and_then(parse_time),
                        ..ProgrammeBuilder::default()
                    });
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
                    if let Some(ch) = channel.take().filter(|c| !c.id.is_empty()) {
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

/// Stores the first non-empty value of each field; later duplicates
/// (e.g. translated titles) are ignored.
fn store_field(
    field: Option<Field>,
    value: String,
    channel: &mut Option<EpgChannel>,
    programme: &mut Option<ProgrammeBuilder>,
) {
    match (field, channel.as_mut(), programme.as_mut()) {
        (Some(Field::DisplayName), Some(ch), _) if ch.display_name.is_empty() => {
            ch.display_name = value;
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
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + min * 60 + sec - offset_secs)
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

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
        assert_eq!(epg.channels[0].display_name, "Example News");
        assert_eq!(
            epg.channels[0].icon.as_deref(),
            Some("http://example.com/news.png")
        );
        assert_eq!(epg.channels[1].icon, None);
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
