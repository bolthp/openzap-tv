//! Optional diagnostic log file (`--debug`).
//!
//! Every line passes through [`mask_urls_in_text`], so stream URLs with
//! access keys never reach the file, whatever the caller writes.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use crate::mask::mask_urls_in_text;

pub struct DebugLog {
    path: PathBuf,
    start: Instant,
    out: Mutex<BufWriter<File>>,
}

impl DebugLog {
    /// Creates (truncates) the log file.
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let file = File::create(path)?;
        Ok(DebugLog {
            path: path.to_path_buf(),
            start: Instant::now(),
            out: Mutex::new(BufWriter::new(file)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes one line, prefixed with seconds since the log was created.
    /// Flushed immediately so the file is useful even after a crash.
    pub fn line(&self, msg: &str) {
        let masked = mask_urls_in_text(msg);
        let elapsed = self.start.elapsed().as_secs_f64();
        // A poisoned lock still holds a usable writer; logging must not panic.
        let mut out = self.out.lock().unwrap_or_else(|p| p.into_inner());
        // Write errors are ignored: diagnostics must never break the app.
        let _ = writeln!(out, "{elapsed:9.3} {masked}");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_timestamped_masked_lines() {
        let path =
            std::env::temp_dir().join(format!("openzap-debuglog-{}.log", std::process::id()));
        let log = DebugLog::create(&path).unwrap();
        log.line("hello");
        log.line("open http://example.com/a?token=secret123");
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with(" hello"));
        assert!(lines[0].trim_start().starts_with("0."));
        assert!(!text.contains("secret123"));
        assert!(text.contains("token=***"));
        std::fs::remove_file(&path).unwrap();
    }
}
