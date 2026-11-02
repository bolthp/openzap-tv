//! Masking of secrets in stream URLs, for safe logging.

/// Query parameter names whose values are treated as secrets (lowercase).
const SENSITIVE_KEYS: &[&str] = &[
    "key",
    "access_key",
    "apikey",
    "api_key",
    "token",
    "auth",
    "password",
    "pass",
    "pwd",
    "username",
    "secret",
    "sig",
    "signature",
    // Per-request signatures (e.g. HLS segment URLs `?md5=...&expires=...`).
    "md5",
    "hash",
    "hmac",
    "sign",
];

/// Path segments that are followed by `<username>/<password>` in Xtream
/// Codes style URLs (`/live/user/pass/123.ts`).
const XTREAM_PREFIXES: &[&str] = &["live", "movie", "series", "timeshift"];

/// Path segments at least this long, mixing letters and digits and without
/// a file extension, are treated as embedded access keys.
const TOKEN_MIN_LEN: usize = 10;

/// Returns `url` with secrets replaced by `***`: credentials (`user:pass@`),
/// sensitive query values, Xtream-style `user/pass` path segments and
/// token-like path segments.
///
/// This is a best-effort heuristic: a short, all-letters key in an unusual
/// place cannot be told apart from an ordinary path segment.
///
/// Takes `&str` (a borrowed view) and returns an owned `String`, because the result is new text.
pub fn mask_url(url: &str) -> String {
    let (main, fragment) = match url.split_once('#') {
        Some((m, f)) => (m, Some(f)),
        None => (url, None),
    };
    let (base, query) = match main.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (main, None),
    };

    let mut out = mask_path(&mask_userinfo(base));
    if let Some(query) = query {
        out.push('?');
        let masked: Vec<String> = query.split('&').map(mask_param).collect();
        out.push_str(&masked.join("&"));
    }
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// Masks every URL found in free text (e.g. a log line) with [`mask_url`].
///
/// A URL starts at an `http(s)://` / `rtmp://` style scheme and ends at
/// whitespace or a quote/bracket character.
pub fn mask_urls_in_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("://") {
        // Walk back over the scheme letters (e.g. "https").
        let scheme_start = rest[..pos]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.'))
            .map_or(0, |i| i + 1);
        if scheme_start == pos {
            // "://" without a scheme: not a URL, copy it and move on.
            out.push_str(&rest[..pos + 3]);
            rest = &rest[pos + 3..];
            continue;
        }
        let end = rest[pos..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | ')' | ']')
            })
            .map_or(rest.len(), |i| pos + i);
        out.push_str(&rest[..scheme_start]);
        out.push_str(&mask_url(&rest[scheme_start..end]));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn mask_userinfo(base: &str) -> String {
    let Some(scheme_end) = base.find("://") else {
        return base.to_string();
    };
    let auth_start = scheme_end + 3;
    let auth_end = base[auth_start..]
        .find('/')
        .map_or(base.len(), |i| auth_start + i);
    match base[auth_start..auth_end].rfind('@') {
        Some(at) => format!("{}***{}", &base[..auth_start], &base[auth_start + at..]),
        None => base.to_string(),
    }
}

/// Masks secrets in the path part of `base` (everything after the host).
fn mask_path(base: &str) -> String {
    let path_start = base
        .find("://")
        .map(|i| i + 3)
        .and_then(|auth| base[auth..].find('/').map(|p| auth + p));
    let Some(path_start) = path_start else {
        return base.to_string();
    };
    let (head, path) = base.split_at(path_start);
    let mut segments: Vec<&str> = path.split('/').collect();

    let mut masked = vec![false; segments.len()];
    for (i, seg) in segments.iter().enumerate() {
        if XTREAM_PREFIXES.iter().any(|p| seg.eq_ignore_ascii_case(p)) && i + 3 < segments.len() {
            masked[i + 1] = true;
            masked[i + 2] = true;
        }
        if looks_like_token(seg) {
            masked[i] = true;
        }
    }
    for (seg, hide) in segments.iter_mut().zip(&masked) {
        if *hide {
            *seg = "***";
        }
    }
    format!("{head}{}", segments.join("/"))
}

fn looks_like_token(segment: &str) -> bool {
    segment.len() >= TOKEN_MIN_LEN
        && !segment.contains('.')
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && segment.bytes().any(|b| b.is_ascii_alphabetic())
        && segment.bytes().any(|b| b.is_ascii_digit())
}

fn mask_param(param: &str) -> String {
    match param.split_once('=') {
        Some((name, _)) if SENSITIVE_KEYS.contains(&name.to_ascii_lowercase().as_str()) => {
            format!("{name}=***")
        }
        _ => param.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_sensitive_query_values() {
        assert_eq!(
            mask_url("http://example.com/live.m3u8?token=abc123&quality=hd"),
            "http://example.com/live.m3u8?token=***&quality=hd"
        );
    }

    #[test]
    fn masking_is_case_insensitive_on_names() {
        assert_eq!(
            mask_url("http://example.com/a?Password=x&USERNAME=y"),
            "http://example.com/a?Password=***&USERNAME=***"
        );
    }

    #[test]
    fn masks_segment_signatures() {
        assert_eq!(
            mask_url("http://example.com/iptv/1/1793631119000.ts?md5=abcDEF123_-x&expires=1"),
            "http://example.com/iptv/1/1793631119000.ts?md5=***&expires=1"
        );
    }

    #[test]
    fn masks_userinfo() {
        assert_eq!(
            mask_url("http://user:fakepass@example.com:8080/stream"),
            "http://***@example.com:8080/stream"
        );
    }

    #[test]
    fn keeps_plain_urls_and_fragments() {
        assert_eq!(
            mask_url("https://example.com/a/b.m3u8#t=5"),
            "https://example.com/a/b.m3u8#t=5"
        );
    }

    #[test]
    fn masks_token_in_path() {
        assert_eq!(
            mask_url("http://example.com/iptv/AB12CD34EF56GH/6387/index.m3u8"),
            "http://example.com/iptv/***/6387/index.m3u8"
        );
    }

    #[test]
    fn masks_xtream_credentials() {
        assert_eq!(
            mask_url("http://example.com:8080/live/john/secret/12345.ts"),
            "http://example.com:8080/live/***/***/12345.ts"
        );
        assert_eq!(
            mask_url("http://example.com/MOVIE/user/pass/99.mkv?x=1"),
            "http://example.com/MOVIE/***/***/99.mkv?x=1"
        );
    }

    #[test]
    fn xtream_prefix_without_enough_segments_is_left_alone() {
        // "/live/news/index.m3u8" is an ordinary path, not user/pass.
        assert_eq!(
            mask_url("http://example.com/live/news/index.m3u8"),
            "http://example.com/live/news/index.m3u8"
        );
    }

    #[test]
    fn ordinary_path_segments_are_kept() {
        for url in [
            "http://example.com/hls/channel1/index.m3u8",
            "http://example.com/streams/12345678901/playlist.m3u8", // digits only
            "http://example.com/streams/onlyletterslong/playlist.m3u8",
            "http://example.com/video_720p.m3u8",
        ] {
            assert_eq!(mask_url(url), url);
        }
    }

    #[test]
    fn everything_combined() {
        assert_eq!(
            mask_url("http://u:p@example.com/live/a/b/1.ts?token=t#frag"),
            "http://***@example.com/live/***/***/1.ts?token=***#frag"
        );
    }

    #[test]
    fn masks_urls_inside_text() {
        assert_eq!(
            mask_urls_in_text("Opening http://example.com/a?token=k1 failed"),
            "Opening http://example.com/a?token=*** failed"
        );
        assert_eq!(
            mask_urls_in_text(
                "two: 'https://u:p@example.com/x' and (http://example.com/live/a/b/1.ts)"
            ),
            "two: 'https://***@example.com/x' and (http://example.com/live/***/***/1.ts)"
        );
    }

    #[test]
    fn text_without_urls_is_unchanged() {
        for text in ["", "plain log line", "ratio 16:9 ://odd", "path C:\\x\\y"] {
            assert_eq!(mask_urls_in_text(text), text);
        }
    }

    #[test]
    fn url_at_end_of_text() {
        assert_eq!(
            mask_urls_in_text("stream=http://example.com/iptv/AB12CD34EF56/1/index.m3u8"),
            "stream=http://example.com/iptv/***/1/index.m3u8"
        );
    }

    #[test]
    fn at_sign_in_path_is_not_userinfo() {
        assert_eq!(
            mask_url("http://example.com/@live/stream"),
            "http://example.com/@live/stream"
        );
    }
}
