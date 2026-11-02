//! Technical details of the playing stream (resolution, codecs, HDR,
//! decoder, bitrate), built from mpv property values.

/// mpv properties [`StreamInfo::from_properties`] reads.
pub const PROPERTIES: &[&str] = &[
    "video-params/w",
    "video-params/h",
    "video-format",
    "container-fps",
    "estimated-vf-fps",
    "video-params/gamma",
    "video-params/primaries",
    "hwdec-current",
    "video-bitrate",
    "audio-codec-name",
    "audio-params/channel-count",
    "audio-params/samplerate",
    "audio-bitrate",
];

/// Transfer characteristic of the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dynamic {
    Sdr,
    /// HDR10-style perceptual quantizer.
    Pq,
    /// Hybrid log-gamma (broadcast HDR).
    Hlg,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// mpv's short codec name, e.g. `hevc`.
    pub video_codec: Option<String>,
    pub fps: Option<f64>,
    pub dynamic: Option<Dynamic>,
    /// Wide colour gamut (BT.2020 primaries).
    pub wide_gamut: bool,
    /// Hardware decoder in use (e.g. `nvdec`); `Some("no")` is the CPU.
    pub hwdec: Option<String>,
    pub video_bitrate: Option<u64>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<u32>,
    pub sample_rate: Option<u32>,
    pub audio_bitrate: Option<u64>,
}

impl StreamInfo {
    /// Builds the info from a property lookup (`get(name)` returns mpv's
    /// text value, or `None` when unavailable).
    pub fn from_properties(get: impl Fn(&str) -> Option<String>) -> Self {
        let text = |name: &str| get(name).filter(|v| !v.is_empty());
        let num = |name: &str| text(name).and_then(|v| v.trim().parse::<f64>().ok());
        let positive = |name: &str| num(name).filter(|v| v.is_finite() && *v > 0.0);
        let dynamic = text("video-params/gamma").map(|g| match g.as_str() {
            "pq" => Dynamic::Pq,
            "hlg" => Dynamic::Hlg,
            _ => Dynamic::Sdr,
        });
        StreamInfo {
            width: positive("video-params/w").map(|v| v as u32),
            height: positive("video-params/h").map(|v| v as u32),
            video_codec: text("video-format"),
            fps: positive("container-fps").or_else(|| positive("estimated-vf-fps")),
            dynamic,
            wide_gamut: text("video-params/primaries").as_deref() == Some("bt.2020"),
            hwdec: text("hwdec-current"),
            video_bitrate: positive("video-bitrate").map(|v| v as u64),
            audio_codec: text("audio-codec-name"),
            audio_channels: positive("audio-params/channel-count").map(|v| v as u32),
            sample_rate: positive("audio-params/samplerate").map(|v| v as u32),
            audio_bitrate: positive("audio-bitrate").map(|v| v as u64),
        }
    }

    /// Human-readable lines, skipping whatever is not known yet.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();

        let mut video = Vec::new();
        if let (Some(w), Some(h)) = (self.width, self.height) {
            video.push(format!("{w}×{h}"));
        }
        if let Some(c) = &self.video_codec {
            video.push(codec_label(c));
        }
        if let Some(fps) = self.fps {
            video.push(format!("{} fps", format_fps(fps)));
        }
        if let Some(d) = self.dynamic {
            video.push(dynamic_label(d, self.wide_gamut).to_string());
        }
        if !video.is_empty() {
            lines.push(format!("Video: {}", video.join(" · ")));
        }

        if let Some(hw) = &self.hwdec {
            lines.push(match hw.as_str() {
                "no" => "Decoding: software (CPU)".to_string(),
                api => format!("Decoding: hardware ({api})"),
            });
        }

        let mut audio = Vec::new();
        if let Some(c) = &self.audio_codec {
            audio.push(codec_label(c));
        }
        if let Some(ch) = self.audio_channels {
            audio.push(channels_label(ch));
        }
        if let Some(rate) = self.sample_rate {
            audio.push(format!("{} kHz", format_fps(f64::from(rate) / 1000.0)));
        }
        if !audio.is_empty() {
            lines.push(format!("Audio: {}", audio.join(" · ")));
        }

        let mut rates = Vec::new();
        if let Some(v) = self.video_bitrate {
            rates.push(format!("video {}", format_bitrate(v)));
        }
        if let Some(a) = self.audio_bitrate {
            rates.push(format!("audio {}", format_bitrate(a)));
        }
        if !rates.is_empty() {
            lines.push(format!("Bitrate: {}", rates.join(" · ")));
        }
        lines
    }
}

/// Display name for an FFmpeg codec name.
fn codec_label(name: &str) -> String {
    match name {
        "h264" => "H.264".into(),
        "hevc" => "HEVC".into(),
        "mpeg2video" => "MPEG-2".into(),
        "mpeg1video" => "MPEG-1".into(),
        "mp2" => "MP2".into(),
        "mp3" | "mp3float" => "MP3".into(),
        "ac3" => "AC-3".into(),
        "eac3" => "E-AC-3".into(),
        "aac" | "aac_latm" => "AAC".into(),
        other => other.to_uppercase(),
    }
}

fn dynamic_label(d: Dynamic, wide_gamut: bool) -> &'static str {
    match (d, wide_gamut) {
        (Dynamic::Pq, _) => "HDR (PQ)",
        (Dynamic::Hlg, _) => "HDR (HLG)",
        (Dynamic::Sdr, true) => "SDR (BT.2020)",
        (Dynamic::Sdr, false) => "SDR",
    }
}

fn channels_label(n: u32) -> String {
    match n {
        1 => "mono".into(),
        2 => "stereo".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        n => format!("{n} ch"),
    }
}

/// `25` → "25", `29.97` → "29.97", `23.976` → "23.98".
fn format_fps(v: f64) -> String {
    let rounded = (v * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded}")
    }
}

/// Bits per second as kbit/s or Mbit/s.
fn format_bitrate(bps: u64) -> String {
    if bps >= 1_000_000 {
        format!("{:.1} Mbit/s", bps as f64 / 1_000_000.0)
    } else {
        format!("{} kbit/s", (bps as f64 / 1000.0).round())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn info(pairs: &[(&str, &str)]) -> StreamInfo {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        StreamInfo::from_properties(|name| map.get(name).map(|v| v.to_string()))
    }

    #[test]
    fn full_hdr_stream() {
        let i = info(&[
            ("video-params/w", "3840"),
            ("video-params/h", "2160"),
            ("video-format", "hevc"),
            ("container-fps", "50.000000"),
            ("video-params/gamma", "hlg"),
            ("video-params/primaries", "bt.2020"),
            ("hwdec-current", "nvdec"),
            ("video-bitrate", "18500000"),
            ("audio-codec-name", "eac3"),
            ("audio-params/channel-count", "6"),
            ("audio-params/samplerate", "48000"),
            ("audio-bitrate", "256000"),
        ]);
        assert_eq!(
            i.lines(),
            [
                "Video: 3840×2160 · HEVC · 50 fps · HDR (HLG)",
                "Decoding: hardware (nvdec)",
                "Audio: E-AC-3 · 5.1 · 48 kHz",
                "Bitrate: video 18.5 Mbit/s · audio 256 kbit/s",
            ]
        );
    }

    #[test]
    fn sdr_software_stream() {
        let i = info(&[
            ("video-params/w", "1920"),
            ("video-params/h", "1080"),
            ("video-format", "h264"),
            ("estimated-vf-fps", "29.970030"),
            ("video-params/gamma", "bt.1886"),
            ("hwdec-current", "no"),
            ("audio-codec-name", "aac"),
            ("audio-params/channel-count", "2"),
            ("audio-params/samplerate", "44100"),
        ]);
        assert_eq!(
            i.lines(),
            [
                "Video: 1920×1080 · H.264 · 29.97 fps · SDR",
                "Decoding: software (CPU)",
                "Audio: AAC · stereo · 44.1 kHz",
            ]
        );
    }

    #[test]
    fn nothing_known_gives_no_lines() {
        assert!(info(&[]).lines().is_empty());
        // Garbage and zeros are treated as unknown.
        let i = info(&[
            ("video-params/w", "0"),
            ("video-params/h", "abc"),
            ("container-fps", "nan"),
            ("video-bitrate", ""),
        ]);
        assert_eq!(i, StreamInfo::default());
    }

    #[test]
    fn pq_and_wide_gamut_sdr() {
        assert_eq!(
            info(&[("video-params/gamma", "pq")]).lines(),
            ["Video: HDR (PQ)"]
        );
        assert_eq!(
            info(&[
                ("video-params/gamma", "bt.1886"),
                ("video-params/primaries", "bt.2020")
            ])
            .lines(),
            ["Video: SDR (BT.2020)"]
        );
    }

    #[test]
    fn reads_only_listed_properties() {
        let asked = std::cell::RefCell::new(Vec::new());
        StreamInfo::from_properties(|name| {
            asked.borrow_mut().push(name.to_string());
            None
        });
        for name in asked.into_inner() {
            assert!(PROPERTIES.contains(&name.as_str()), "{name}");
        }
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(format_fps(25.0), "25");
        assert_eq!(format_fps(23.976), "23.98");
        assert_eq!(format_bitrate(999_499), "999 kbit/s");
        assert_eq!(format_bitrate(4_260_000), "4.3 Mbit/s");
        assert_eq!(codec_label("vp9"), "VP9");
        assert_eq!(channels_label(3), "3 ch");
    }
}
