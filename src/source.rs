//! Loading playlist/EPG text from a local file or an HTTP(S) URL.
//!
//! Gzip data is detected by its magic bytes, so `.xml.gz` files work
//! whatever their name or content type. Error messages never contain the
//! URL: it may carry an access key.

use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

/// Upper bound for both the download and the decompressed text.
const MAX_BYTES: u64 = 256 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    File(PathBuf),
    Url(String),
}

impl Source {
    /// `http://` and `https://` prefixes mean a URL, anything else is a file path.
    pub fn parse(input: &str) -> Self {
        let input = input.trim();
        let lower = input.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            Source::Url(input.to_string())
        } else {
            Source::File(PathBuf::from(input))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// Local file problem (message contains the path, never a URL).
    File(String),
    /// The server answered with an error status.
    Status(u16),
    Timeout,
    HostNotFound,
    /// Any other network problem.
    Network,
    TooLarge,
    /// Corrupt gzip data.
    Decompress,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::File(msg) => write!(f, "{msg}"),
            FetchError::Status(code) => write!(f, "server answered HTTP {code}"),
            FetchError::Timeout => write!(f, "request timed out"),
            FetchError::HostNotFound => write!(f, "host not found"),
            FetchError::Network => write!(f, "network error"),
            FetchError::TooLarge => write!(f, "file is too large"),
            FetchError::Decompress => write!(f, "cannot decompress gzip data"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Fetches the source and returns its text (gunzipped if needed).
pub fn fetch_text(source: &Source) -> Result<String, FetchError> {
    let bytes = match source {
        Source::File(path) => read_file(path)?,
        Source::Url(url) => download(url)?,
    };
    let bytes = gunzip_if_needed(bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_file(path: &PathBuf) -> Result<Vec<u8>, FetchError> {
    let file = std::fs::File::open(path)
        .map_err(|e| FetchError::File(format!("cannot read {}: {}", path.display(), e.kind())))?;
    read_limited(file).map_err(|e| match e {
        ReadError::TooLarge => FetchError::TooLarge,
        ReadError::Io(kind) => FetchError::File(format!("cannot read {}: {kind}", path.display())),
    })
}

fn download(url: &str) -> Result<Vec<u8>, FetchError> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("OpenZap/", env!("CARGO_PKG_VERSION")))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent.get(url).call().map_err(map_http_error)?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_vec()
        .map_err(map_http_error)
}

/// Translates ureq errors into messages that cannot leak the URL.
fn map_http_error(err: ureq::Error) -> FetchError {
    match err {
        ureq::Error::StatusCode(code) => FetchError::Status(code),
        ureq::Error::Timeout(_) => FetchError::Timeout,
        ureq::Error::HostNotFound => FetchError::HostNotFound,
        ureq::Error::BodyExceedsLimit(_) => FetchError::TooLarge,
        _ => FetchError::Network,
    }
}

enum ReadError {
    TooLarge,
    Io(std::io::ErrorKind),
}

/// Reads everything, failing if more than `MAX_BYTES` come out.
fn read_limited(reader: impl Read) -> Result<Vec<u8>, ReadError> {
    let mut buf = Vec::new();
    // Read one byte past the limit to tell "exactly at limit" from "too big".
    reader
        .take(MAX_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| ReadError::Io(e.kind()))?;
    if buf.len() as u64 > MAX_BYTES {
        return Err(ReadError::TooLarge);
    }
    Ok(buf)
}

fn gunzip_if_needed(bytes: Vec<u8>) -> Result<Vec<u8>, FetchError> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(bytes);
    }
    read_limited(flate2::read::GzDecoder::new(bytes.as_slice())).map_err(|e| match e {
        ReadError::TooLarge => FetchError::TooLarge,
        ReadError::Io(_) => FetchError::Decompress,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use std::net::TcpListener;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    /// Serves one canned HTTP response on 127.0.0.1 and returns the base URL.
    fn serve_once(status_line: &'static str, body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read the request head so the client isn't cut off mid-write.
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                head.push(byte[0]);
            }
            let header = format!(
                "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        format!("http://{addr}/list.m3u8")
    }

    fn temp_file(name: &str, data: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openzap-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        path
    }

    #[test]
    fn source_parse_detects_urls_and_paths() {
        assert_eq!(
            Source::parse("  https://example.com/a.m3u "),
            Source::Url("https://example.com/a.m3u".into())
        );
        assert_eq!(
            Source::parse("HTTP://example.com/a"),
            Source::Url("HTTP://example.com/a".into())
        );
        assert_eq!(
            Source::parse("/home/me/list.m3u"),
            Source::File("/home/me/list.m3u".into())
        );
        assert_eq!(
            Source::parse("C:\\lists\\a.m3u"),
            Source::File("C:\\lists\\a.m3u".into())
        );
    }

    #[test]
    fn reads_plain_file() {
        let path = temp_file("plain.m3u", b"#EXTM3U\n");
        assert_eq!(fetch_text(&Source::File(path)).unwrap(), "#EXTM3U\n");
    }

    #[test]
    fn reads_gzipped_file_regardless_of_name() {
        let path = temp_file("looks-plain.xml", &gzip(b"<tv></tv>"));
        assert_eq!(fetch_text(&Source::File(path)).unwrap(), "<tv></tv>");
    }

    #[test]
    fn missing_file_error_names_the_path() {
        let err = fetch_text(&Source::File("/nonexistent/openzap.m3u".into())).unwrap_err();
        assert!(matches!(&err, FetchError::File(m) if m.contains("/nonexistent/openzap.m3u")));
    }

    #[test]
    fn corrupt_gzip_is_an_error() {
        let path = temp_file("corrupt.gz", &[0x1f, 0x8b, 0x08, 0x00, 1, 2, 3]);
        assert_eq!(fetch_text(&Source::File(path)), Err(FetchError::Decompress));
    }

    #[test]
    fn downloads_text() {
        let url = serve_once("200 OK", b"#EXTM3U\n".to_vec());
        assert_eq!(fetch_text(&Source::Url(url)).unwrap(), "#EXTM3U\n");
    }

    #[test]
    fn downloads_and_gunzips() {
        let url = serve_once("200 OK", gzip(b"<tv></tv>"));
        assert_eq!(fetch_text(&Source::Url(url)).unwrap(), "<tv></tv>");
    }

    #[test]
    fn http_error_status_is_reported() {
        let url = serve_once("404 Not Found", b"nope".to_vec());
        assert_eq!(fetch_text(&Source::Url(url)), Err(FetchError::Status(404)));
    }

    #[test]
    fn errors_never_contain_the_url() {
        let url = format!("{}?token=fakekey123", serve_once("403 Forbidden", vec![]));
        let err = fetch_text(&Source::Url(url)).unwrap_err().to_string();
        assert!(!err.contains("fakekey123"), "leaked: {err}");
        assert!(!err.contains("127.0.0.1"), "leaked: {err}");
    }

    #[test]
    fn connection_refused_is_a_network_error() {
        // Bind then drop to get a port nothing listens on.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = fetch_text(&Source::Url(format!("http://127.0.0.1:{port}/x"))).unwrap_err();
        assert!(matches!(err, FetchError::Network | FetchError::Timeout));
    }
}
