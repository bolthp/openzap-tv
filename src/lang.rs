//! Preferred audio and subtitle languages, as typed in the settings.
//!
//! Streams tag their tracks with two- or three-letter codes ("ru", "rus",
//! "de", "ger", "deu"), and mpv compares the tags as they are. So each
//! language typed is expanded to all its common codes.

/// ISO 639-1 code and its ISO 639-2 codes (bibliographic one first where
/// it differs). Languages common in IPTV playlists; others pass through.
const CODES: &[(&str, &[&str])] = &[
    ("ar", &["ara"]),
    ("az", &["aze"]),
    ("be", &["bel"]),
    ("bg", &["bul"]),
    ("bs", &["bos"]),
    ("ca", &["cat"]),
    ("cs", &["cze", "ces"]),
    ("da", &["dan"]),
    ("de", &["ger", "deu"]),
    ("el", &["gre", "ell"]),
    ("en", &["eng"]),
    ("es", &["spa"]),
    ("et", &["est"]),
    ("fa", &["per", "fas"]),
    ("fi", &["fin"]),
    ("fr", &["fre", "fra"]),
    ("he", &["heb"]),
    ("hi", &["hin"]),
    ("hr", &["hrv"]),
    ("hu", &["hun"]),
    ("hy", &["arm", "hye"]),
    ("id", &["ind"]),
    ("it", &["ita"]),
    ("ja", &["jpn"]),
    ("ka", &["geo", "kat"]),
    ("kk", &["kaz"]),
    ("ko", &["kor"]),
    ("lt", &["lit"]),
    ("lv", &["lav"]),
    ("mk", &["mac", "mkd"]),
    ("nl", &["dut", "nld"]),
    ("no", &["nor"]),
    ("pl", &["pol"]),
    ("pt", &["por"]),
    ("ro", &["rum", "ron"]),
    ("ru", &["rus"]),
    ("sk", &["slo", "slk"]),
    ("sl", &["slv"]),
    ("sq", &["alb", "sqi"]),
    ("sr", &["srp"]),
    ("sv", &["swe"]),
    ("th", &["tha"]),
    ("tr", &["tur"]),
    ("uk", &["ukr"]),
    ("uz", &["uzb"]),
    ("vi", &["vie"]),
    ("zh", &["chi", "zho"]),
];

/// Why a language list cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangError(String);

impl std::fmt::Display for LangError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "\"{}\" is not a language code (use codes like ru, en or deu, separated by commas)",
            self.0
        )
    }
}

/// The codes typed, in order: "ru, EN" → ["ru", "en"]. Empty text is an
/// empty list (no preference).
pub fn parse(text: &str) -> Result<Vec<String>, LangError> {
    text.split(',')
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(|code| {
            // Letters with an optional region: "pt-BR".
            let valid = code.len() <= 12
                && code.starts_with(|c: char| c.is_ascii_alphabetic())
                && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            if valid {
                Ok(code.to_ascii_lowercase())
            } else {
                Err(LangError(code.to_string()))
            }
        })
        .collect()
}

/// The value of mpv's `alang` / `slang` for `text`: every code with its
/// other forms right after it, without repeats ("ru" → "ru,rus").
/// Invalid text gives no preference.
pub fn mpv_value(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let codes = parse(text).unwrap_or_default();
    for code in &codes {
        // "pt-br": its own tag first, then the plain language.
        let base = code.split('-').next().unwrap_or(code);
        let family = CODES
            .iter()
            .find(|(two, three)| *two == base || three.contains(&base));
        let mut add = |c: &str| {
            if !out.iter().any(|o| o == c) {
                out.push(c.to_string());
            }
        };
        add(code);
        if let Some((two, three)) = family {
            add(two);
            for c in *three {
                add(c);
            }
        }
    }
    out.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_comma_list() {
        assert_eq!(parse(""), Ok(vec![]));
        assert_eq!(parse(" , "), Ok(vec![]));
        assert_eq!(
            parse("ru, EN,pt-BR"),
            Ok(vec!["ru".into(), "en".into(), "pt-br".into()])
        );
        assert!(parse("russian!").is_err());
        assert!(parse("ru en").is_err());
        assert!(parse("1ru").is_err());
        let message = parse("ru;en").unwrap_err().to_string();
        assert!(message.contains("\"ru;en\""), "{message}");
    }

    #[test]
    fn expands_to_all_forms() {
        assert_eq!(mpv_value("ru"), "ru,rus");
        assert_eq!(mpv_value("rus"), "rus,ru");
        assert_eq!(mpv_value("deu, en"), "deu,de,ger,en,eng");
        assert_eq!(mpv_value("de"), "de,ger,deu");
        // Repeats are dropped; unknown codes pass through.
        assert_eq!(mpv_value("ru, rus, xx"), "ru,rus,xx");
        assert_eq!(mpv_value("pt-BR"), "pt-br,pt,por");
        assert_eq!(mpv_value(""), "");
        assert_eq!(mpv_value("bad!"), "");
    }
}
