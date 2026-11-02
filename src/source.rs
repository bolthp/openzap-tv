//! Reading playlist/EPG data from a local file or an HTTP(S) URL.
//!
//! Data is streamed: [`open`] returns a reader that decompresses on the
//! fly, so large guides never have to sit in memory as one block. Gzip is
//! detected by its magic bytes, so `.xml.gz` works whatever the name or
//! content type. Error messages never contain the URL: it may carry an
//! access key.

use std::fmt;
use std::io::{self, BufRead, BufReader, ErrorKind, Read};
use std::path::PathBuf;
use std::time::Duration;

/// Default limit of decompressed bytes for [`fetch_text`].
pub const MAX_TEXT_BYTES: u64 = 256 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// No data for this long on an open connection means it is stuck. There is
/// deliberately no total timeout: a big guide on a slow line may take minutes.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

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

/// A source opened for streaming.
pub struct Opened {
    /// Yields the decompressed bytes; fails with `ErrorKind::FileTooLarge`
    /// once more than the requested maximum comes out.
    pub reader: Box<dyn BufRead + Send>,
    /// The data was gzip-compressed.
    pub compressed: bool,
}

/// Opens the source for streaming, gunzipping on the fly if needed.
pub fn open(source: &Source, max_bytes: u64) -> Result<Opened, FetchError> {
    let raw: Box<dyn Read + Send> = match source {
        Source::File(path) => {
            Box::new(std::fs::File::open(path).map_err(|e| file_error(path, e.kind()))?)
        }
        Source::Url(url) => Box::new(connect(url)?),
    };
    let mut buffered = BufReader::new(raw);
    let compressed = buffered
        .fill_buf()
        .map_err(|e| read_error(source, e.kind(), false))?
        .starts_with(&[0x1f, 0x8b]);
    let decoded: Box<dyn Read + Send> = if compressed {
        // Multi-member: some servers concatenate several gzip streams.
        Box::new(flate2::read::MultiGzDecoder::new(buffered))
    } else {
        Box::new(buffered)
    };
    Ok(Opened {
        reader: Box::new(BufReader::with_capacity(
            64 * 1024,
            Limited {
                inner: decoded,
                remaining: max_bytes,
            },
        )),
        compressed,
    })
}

/// Fetches the whole source as text (for playlists; guides should stream).
pub fn fetch_text(source: &Source) -> Result<String, FetchError> {
    let bytes = fetch_bytes(source, MAX_TEXT_BYTES)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Fetches the whole source as bytes, up to `max_bytes` (e.g. images).
pub fn fetch_bytes(source: &Source, max_bytes: u64) -> Result<Vec<u8>, FetchError> {
    let mut opened = open(source, max_bytes)?;
    let mut bytes = Vec::new();
    opened
        .reader
        .read_to_end(&mut bytes)
        .map_err(|e| read_error(source, e.kind(), opened.compressed))?;
    Ok(bytes)
}

/// Turns an I/O error that happened while reading `source` into a message
/// that cannot leak the URL.
pub fn read_error(source: &Source, kind: ErrorKind, compressed: bool) -> FetchError {
    match kind {
        ErrorKind::FileTooLarge => FetchError::TooLarge,
        ErrorKind::TimedOut => FetchError::Timeout,
        ErrorKind::InvalidData | ErrorKind::InvalidInput | ErrorKind::UnexpectedEof
            if compressed =>
        {
            FetchError::Decompress
        }
        _ => match source {
            Source::File(path) => file_error(path, kind),
            Source::Url(_) => FetchError::Network,
        },
    }
}

fn file_error(path: &std::path::Path, kind: ErrorKind) -> FetchError {
    FetchError::File(format!("cannot read {}: {kind}", path.display()))
}

fn connect(url: &str) -> Result<impl Read + Send + use<>, FetchError> {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_per_read(Some(READ_TIMEOUT))
        .user_agent(concat!("OpenZap/", env!("CARGO_PKG_VERSION")))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let response = agent.get(url).call().map_err(map_http_error)?;
    // Unlimited here; `Limited` in `open` enforces the size.
    Ok(response.into_body().into_reader())
}

/// Translates ureq errors into messages that cannot leak the URL.
fn map_http_error(err: ureq::Error) -> FetchError {
    match err {
        ureq::Error::StatusCode(code) => FetchError::Status(code),
        ureq::Error::Timeout(_) => FetchError::Timeout,
        ureq::Error::HostNotFound => FetchError::HostNotFound,
        _ => FetchError::Network,
    }
}

/// Passes at most `remaining` bytes through, then fails if more would follow.
struct Limited<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            // At the limit: fine if the data ends exactly here.
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    ErrorKind::FileTooLarge,
                    "data exceeds the size limit",
                )),
            };
        }
        let max = buf
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..max])?;
        self.remaining -= n as u64;
        Ok(n)
    }
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
    fn limit_is_enforced_while_streaming() {
        let path = temp_file("big.txt", &[b'x'; 100]);
        let mut opened = open(&Source::File(path.clone()), 10).unwrap();
        let mut out = Vec::new();
        let err = opened.reader.read_to_end(&mut out).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::FileTooLarge);
        assert_eq!(
            read_error(&Source::File(path), err.kind(), false),
            FetchError::TooLarge
        );
    }

    #[test]
    fn data_exactly_at_the_limit_is_fine() {
        let path = temp_file("exact.txt", &[b'y'; 10]);
        let mut opened = open(&Source::File(path), 10).unwrap();
        let mut out = Vec::new();
        opened.reader.read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn limit_applies_to_decompressed_size() {
        // 1 MB of zeros compresses to about 1 KB: a small download, a big result.
        let path = temp_file("bomb.gz", &gzip(&vec![0u8; 1024 * 1024]));
        let mut opened = open(&Source::File(path), 64 * 1024).unwrap();
        assert!(opened.compressed);
        let err = opened.reader.read_to_end(&mut Vec::new()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::FileTooLarge);
    }

    #[test]
    fn concatenated_gzip_members_are_all_read() {
        let mut data = gzip(b"first,");
        data.extend(gzip(b"second"));
        let path = temp_file("multi.gz", &data);
        assert_eq!(fetch_text(&Source::File(path)).unwrap(), "first,second");
    }

    #[test]
    fn streams_over_http() {
        let url = serve_once("200 OK", gzip(b"<tv></tv>"));
        let mut opened = open(&Source::Url(url), 1024).unwrap();
        let mut text = String::new();
        opened.reader.read_to_string(&mut text).unwrap();
        assert_eq!(text, "<tv></tv>");
        assert!(opened.compressed);
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
