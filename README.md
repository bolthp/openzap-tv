# OpenZap

Desktop IPTV player for Windows, Linux and macOS: HLS/HTTP streams, M3U playlists, XMLTV EPG.

**Status:** early development.

## Features

- Open an M3U playlist and an optional XMLTV guide from a file or an HTTP(S) URL (`.gz` is supported)
- Search, filter by group, favorites
- "On air now" and "next" for every channel
- Video plays inside the main window (libmpv, OpenGL)
- Channel list on the left or right (or hidden); the programme guide and the
  controls slide up over the video
- Audio track switching, picture size (aspect ratio, crop, stretch), always on top
- Dark, light or system theme
- Channel logos (`tvg-logo` from the playlist, otherwise the guide's channel
  icon), cached on disk for a week
- Programme times in local time
- Programme guide per channel: every programme by day, with descriptions
- Stream info: resolution, codecs, frame rate, HDR, hardware/software decoding, bitrate

## Install

Download an archive from the [Releases](../../releases) page.

- **Windows:** unpack the zip and run `openzap.exe`. `libmpv-2.dll` is included.
  The executable is not code-signed, so Windows SmartScreen may warn on first start.
- **Linux:** unpack the tar.gz and run `./openzap`. libmpv must be installed:
  `sudo apt install libmpv2` (Debian/Ubuntu; `libmpv1` on older releases).

OpenZap looks for libmpv next to the executable first, then in the system.

On Linux, OpenZap adds itself to the application menu on first start
(`~/.local/share/applications/openzap.desktop` and its icon), so launchers and
the taskbar show its icon; the entry follows the executable if you move it.
Delete those two files to remove it.

On Wayland (the default on Ubuntu) an application cannot keep its own window
on top; use the window menu instead (`Alt+Space` or right-click the title bar →
*Always on Top*).

## Usage

On first start the **Settings** dialog opens: enter the playlist (a URL, or
a file chosen with **Browse…**) and, optionally, the programme guide. Settings also hold hardware
decoding, the network buffer and whether to resume the last channel on start.

```sh
openzap                                   # sources from the settings
openzap playlist.m3u [epg.xml]            # or override them for this run
```

Settings (sources, volume, favorites, playback options) are stored in a
plain-text file: `~/.config/openzap/config.txt` on Linux,
`%APPDATA%\OpenZap\config.txt` on Windows. It can contain URLs with access
keys, so keep it private. Logos are cached in `~/.cache/openzap/logos` on Linux and
`%LOCALAPPDATA%\OpenZap\cache\logos` on Windows.

## Keyboard

| Key | Action |
|---|---|
| click / double-click a channel | select / play it (right-click: favorites) |
| `Enter` | play the selected channel |
| digits | jump to the channel with that number (`tvg-chno`, or its position) |
| `Backspace` | back to the previous channel |
| `PageUp` / `PageDown` | previous / next channel |
| `F` / double-click video | fullscreen |
| `Esc` | leave fullscreen |
| `M` / mouse wheel over video | mute / volume |
| `L` | show / hide the channel list (fullscreen: keep it open) |
| `A` | next audio track (when the stream has several) |
| `Z` | picture size: auto, 16:9, 4:3, fill (crop), stretch |
| `T` | always on top |
| `R` | restart the stream (also after "no picture for 20 s") |
| `G` | programme guide of the channel (`Esc` closes it) |
| `I` | show / hide stream info (resolution, codecs, HDR, decoder, bitrate) |

Moving the mouse over the video brings up the bar with the programme guide
and the controls; it hides again after a few seconds.
In fullscreen (or with the list hidden), moving the mouse to the window edge
where the list belongs slides it in over the video; it slides away when the
mouse leaves it, unless pinned with `L` or the list button.

## Troubleshooting

Start with `--debug` (on Windows: double-click `openzap-debug.bat`) to write
`openzap-debug.log` next to the executable: GPU and OpenGL version, player
events, mpv's messages and a periodic check whether video frames arrive.
Stream URLs in the log are masked, but it may still contain channel names and
server host names — look through it before sharing.

## Build from source

Requires a stable Rust toolchain. libmpv is loaded at run time, so it is
not needed to build, only to run (and to run the playback tests).

```sh
cargo run --release
cargo test
```

## Third-party software

Windows releases bundle `libmpv-2.dll` from
[shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake)
(a GPL build of [mpv](https://mpv.io) with FFmpeg); the exact version is pinned in
`scripts/fetch-libmpv-windows.ps1`. Sources are available from that project and from
the mpv and FFmpeg repositories.

## License

GPL-3.0-or-later — see [LICENSE](LICENSE).
