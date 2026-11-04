# TODO

Backlog, roughly in priority order. Move items to a commit message when done.

## Playback

- [ ] **Hardware decoding** is on by default (`auto-safe`, falls back to the
      CPU). Verified: Windows + NVIDIA (nvdec, 4K HEVC). Still to check:
      Windows Intel/AMD, Linux VAAPI/NVIDIA (VAAPI may need the X11/Wayland
      display passed to mpv's renderer for zero-copy).
- [ ] Check `PageUp` / `PageDown` channel switching on Windows (not tested yet).

## Inspired by OTT-Play

Feature set to aim for (see the OTT-Play player for TVs); roughly by value.

- [ ] **Archive (catch-up)**: play past programmes from the guide. Playlists
      mark it with `tvg-rec="<days>"` or `catchup`/`catchup-days`/
      `catchup-source`; URL schemes differ (flussonic `index-<start>-<dur>`,
      `?utc=&lutc=` shift, `append`). Needs per-scheme URL building + tests.
- [ ] Archive marks in the programme guide (once archive exists).
- [ ] **Timeshift** on archive channels: pause live, jump to the programme
      start, seek ±15 s / 1 / 3 / 10 min, previous/next programme, back to live.
- [ ] `I` cycles: channel + programme → programme description → stream info → off.
- [ ] Fullscreen-first navigation: channel list and group picker as overlays
      over the video, fully usable from the keyboard (and a remote).
- [ ] Several playlists (providers) at once.
- [ ] Parental control: PIN for chosen groups/channels.
- [ ] Picture-in-picture (second player instance) — later.

## Programme guide

- [ ] Refresh the guide automatically (daily); data is kept for 4 days ahead.

## Interface

- [ ] Design pass: own font (e.g. Inter) and an icon font (e.g. Phosphor via
      `egui-phosphor`) instead of emoji-style symbols, spacing, a custom
      dark/light palette, hover/press animations.
- [ ] Per-channel picture size.
- [ ] Toolkit question (decided: stay on egui for now). Alternatives looked at:
      Slint (declarative, polished widgets, can render under an OpenGL
      underlay), iced (Elm-style), Tauri/Dioxus (web UI — embedding mpv's
      video is much harder there). egui fits the mpv OpenGL embedding well
      and can look good with styling; revisit only if styling hits limits.

## Settings

- [ ] Proxy (see Network) goes into the Settings dialog.
- [ ] Possibly: audio/subtitle language preference, deinterlacing.

## Network

- [ ] Proxy support (HTTP/SOCKS) for playlist, EPG and streams — mpv has
      `http-proxy`, ureq supports proxies. Today streams that need it only
      work through a system VPN.

## Logos

- [ ] Limit the logo textures kept in memory (LRU) for very long lists.
- [ ] A "clear cache" button in Settings.

## Testing

- Public playlists for manual tests (no keys, many formats and odd names):
  <https://github.com/iptv-org/iptv/tree/master/streams>. Never commit
  downloaded copies; unit tests keep using synthetic `example.com` data.

## Packaging

- [ ] Linux AppImage bundling libmpv (today the system `libmpv2` is required).
      Also fixes the generic taskbar icon on the very first start (the desktop
      entry is written during that start).
- [ ] macOS build, once someone can test it.
