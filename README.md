# OpenZap

Desktop IPTV player for Windows, Linux and macOS: HLS/HTTP streams, M3U playlists, XMLTV EPG.

**Status:** early development.

## Features

- Open an M3U playlist and an optional XMLTV guide from a file or an HTTP(S) URL (`.gz` is supported)
- Search, filter by group, favorites
- "On air now" and "next" for every channel
- Video plays inside the main window (libmpv, OpenGL)

## Install

Download an archive from the [Releases](../../releases) page.

- **Windows:** unpack the zip and run `openzap.exe`. `libmpv-2.dll` is included.
  The executable is not code-signed, so Windows SmartScreen may warn on first start.
- **Linux:** unpack the tar.gz and run `./openzap`. libmpv must be installed:
  `sudo apt install libmpv2` (Debian/Ubuntu; `libmpv1` on older releases).

OpenZap looks for libmpv next to the executable first, then in the system.

## Usage

```sh
openzap                                   # type or paste sources in the window
openzap playlist.m3u [epg.xml]            # or pass them as arguments
```

Settings (last sources, volume, favorites) are stored in a plain-text file:
`~/.config/openzap/config.txt` on Linux, `%APPDATA%\OpenZap\config.txt` on Windows.
It can contain URLs with access keys, so keep it private.

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
