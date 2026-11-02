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
];

/// Returns `url` with credentials (`user:pass@`) and sensitive query values replaced by `***`.
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

    let mut out = mask_userinfo(base);
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
    fn at_sign_in_path_is_not_userinfo() {
        assert_eq!(
            mask_url("http://example.com/@live/stream"),
            "http://example.com/@live/stream"
        );
    }
}
