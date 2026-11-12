//! Reading problems out of mpv's log: HTTP errors and audio that cannot
//! be decoded. Only fixed phrases are matched; URLs in the text are never
//! kept.

use std::collections::VecDeque;

/// The HTTP status of a failed request, from a log line such as
/// `[curl] error: HTTP error 502`.
pub fn http_status(prefix: &str, text: &str) -> Option<u16> {
    if prefix != "curl" && prefix != "stream" {
        return None;
    }
    let code = text.trim().strip_prefix("HTTP error ")?;
    code.split_whitespace().next()?.parse().ok()
}

/// A log line saying a packet of audio could not be decoded.
pub fn is_audio_decode_error(prefix: &str, text: &str) -> bool {
    prefix == "ad" && text.starts_with("Error decoding audio")
}

/// Audio errors in this many seconds…
pub const AUDIO_ERROR_WINDOW_SECS: f64 = 5.0;
/// …this many times mean the audio is broken, not just a glitch.
pub const AUDIO_ERROR_LIMIT: usize = 20;

/// Counts errors in a sliding time window.
#[derive(Debug, Default)]
pub struct ErrorBurst {
    times: VecDeque<f64>,
}

impl ErrorBurst {
    /// Records an error at `time` (seconds); true once there have been
    /// [`AUDIO_ERROR_LIMIT`] within [`AUDIO_ERROR_WINDOW_SECS`].
    pub fn record(&mut self, time: f64) -> bool {
        self.times.push_back(time);
        while self
            .times
            .front()
            .is_some_and(|&t| time - t > AUDIO_ERROR_WINDOW_SECS)
        {
            self.times.pop_front();
        }
        self.times.len() >= AUDIO_ERROR_LIMIT
    }

    pub fn clear(&mut self) {
        self.times.clear();
    }
}

/// Why playback failed, for the user. `msg` is mpv's reason ("loading
/// failed"), `http` the server's answer if one was logged, `archive` that
/// a programme from the archive was asked for.
pub fn failure_text(msg: &str, http: Option<u16>, archive: bool) -> String {
    match (archive, http) {
        (true, Some(code)) => format!(
            "the provider's archive did not answer (HTTP {code}). \
             This programme may not be recorded"
        ),
        (true, None) => format!("the provider's archive did not answer ({msg})"),
        (false, Some(code)) => format!("{msg} (the server answered HTTP {code})"),
        (false, None) => msg.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_http_status() {
        assert_eq!(http_status("curl", "HTTP error 502"), Some(502));
        assert_eq!(http_status("stream", " HTTP error 404 "), Some(404));
        assert_eq!(
            http_status("curl", "TLS certificate verification failed."),
            None
        );
        assert_eq!(http_status("cplayer", "HTTP error 502"), None);
        assert_eq!(http_status("curl", "HTTP error x"), None);
    }

    #[test]
    fn recognises_audio_decode_errors() {
        assert!(is_audio_decode_error("ad", "Error decoding audio."));
        assert!(!is_audio_decode_error("vd", "Error decoding audio."));
        assert!(!is_audio_decode_error("ad", "Selected decoder: aac"));
    }

    #[test]
    fn a_burst_counts_only_recent_errors() {
        let mut burst = ErrorBurst::default();
        // Sporadic glitches over a long time never add up.
        for i in 0..100 {
            assert!(!burst.record(f64::from(i) * 10.0));
        }
        burst.clear();
        let hits: Vec<bool> = (0..AUDIO_ERROR_LIMIT)
            .map(|i| burst.record(i as f64 * 0.1))
            .collect();
        assert!(!hits[AUDIO_ERROR_LIMIT - 2]);
        assert!(hits[AUDIO_ERROR_LIMIT - 1]);
    }

    #[test]
    fn failure_texts() {
        assert_eq!(
            failure_text("loading failed", None, false),
            "loading failed"
        );
        assert_eq!(
            failure_text("loading failed", Some(404), false),
            "loading failed (the server answered HTTP 404)"
        );
        assert!(failure_text("loading failed", Some(502), true).contains("archive"));
        assert!(failure_text("loading failed", Some(502), true).contains("502"));
        assert!(failure_text("loading failed", None, true).contains("loading failed"));
    }
}
