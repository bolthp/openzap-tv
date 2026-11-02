# TODO

Backlog, roughly in priority order. Move items to a commit message when done.

## Playback

- [ ] **Hardware decoding** is on by default (`auto-safe`, falls back to the
      CPU). Verified: Windows + NVIDIA (nvdec, 4K HEVC). Still to check:
      Windows Intel/AMD, Linux VAAPI/NVIDIA (VAAPI may need the X11/Wayland
      display passed to mpv's renderer for zero-copy).
- [ ] Give up on streams that never deliver a frame (e.g. 20 s in "Loading...")
      with a clear message instead of waiting forever.
- [ ] Check `PageUp` / `PageDown` channel switching on Windows (not tested yet).

## Inspired by OTT-Play

Feature set to aim for (see the OTT-Play player for TVs); roughly by value.

- [ ] **Archive (catch-up)**: play past programmes from the guide. Playlists
      mark it with `tvg-rec="<days>"` or `catchup`/`catchup-days`/
      `catchup-source`; URL schemes differ (flussonic `index-<start>-<dur>`,
      `?utc=&lutc=` shift, `append`). Needs per-scheme URL building + tests.
- [ ] **Timeshift** on archive channels: pause live, jump to the programme
      start, seek ±15 s / 1 / 3 / 10 min, previous/next programme, back to live.
- [ ] **Programme list** per channel (past with archive marks, now, upcoming);
      replaces "Day schedule" below.
- [ ] Channel numbers, digit entry to jump, previous channel (`Backspace`).
- [ ] `I` cycles: channel + programme → programme description → stream info → off.
- [ ] Subtitle switching (`S`); restart stream.
- [ ] Fullscreen-first navigation: channel list and group picker as overlays
      over the video, fully usable from the keyboard (and a remote).
- [ ] Several playlists (providers) at once.
- [ ] Parental control: PIN for chosen groups/channels.
- [ ] Picture-in-picture (second player instance) — later.

## Programme guide

- [ ] Show times in the local time zone (needs a time-zone crate).
- [ ] Day schedule for a channel, not only now/next.
- [ ] Match all `display-name` variants of EPG channels (about half of the test
      playlist's channels currently find no EPG).
- [ ] Refresh the guide automatically (daily); data is kept for 4 days ahead.

## Interface

- [ ] Design pass: own font (e.g. Inter) and an icon font (e.g. Phosphor via
      `egui-phosphor`) instead of emoji-style symbols, spacing, a custom
      dark/light palette, hover/press animations.
- [ ] Channel logos (`tvg-logo`) in the list and the bar (download + cache).
- [ ] Remember the window size/position; per-channel picture size.
- [ ] Toolkit question (decided: stay on egui for now). Alternatives looked at:
      Slint (declarative, polished widgets, can render under an OpenGL
      underlay), iced (Elm-style), Tauri/Dioxus (web UI — embedding mpv's
      video is much harder there). egui fits the mpv OpenGL embedding well
      and can look good with styling; revisit only if styling hits limits.
- [ ] File picker for playlist/EPG instead of typing paths.

## Settings

- [ ] Proxy (see Network) goes into the Settings dialog.
- [ ] Possibly: audio/subtitle language preference, deinterlacing.

## Network

- [ ] Proxy support (HTTP/SOCKS) for playlist, EPG and streams — mpv has
      `http-proxy`, ureq supports proxies. Today streams that need it only
      work through a system VPN.

## Packaging

- [ ] Linux AppImage bundling libmpv (today the system `libmpv2` is required).
- [ ] macOS build, once someone can test it.
