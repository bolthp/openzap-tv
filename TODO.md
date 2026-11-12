# TODO

Backlog, roughly in priority order. Move items to a commit message when done.

## Playback

- [ ] **Hardware decoding** (on by default, `auto-safe`, falls back to the
      CPU) works on Windows + NVIDIA (nvdec, also 4K HEVC). Not yet tried:
      Windows with Intel/AMD graphics, Linux with VAAPI (Intel/AMD) or NVIDIA;
      the Steam Deck covers AMD/VAAPI. On Linux, VAAPI may need the X11 or
      Wayland display passed to mpv's renderer (not done yet) to decode
      without copying frames.

## Inspired by OTT-Play

Feature set to aim for (see the OTT-Play player for TVs); roughly by value.

- [ ] Archive: shift (`tvg-rec`) works with a real provider; timeshift is
      only tested against a local server so far. Flussonic and templates are only tested against synthetic
      URLs. Flussonic "watch from the start"
      of a running programme asks for its full length; check the server's
      answer. Timeshift keys (Space, arrows, Home, End) not yet tried on
      Windows. Xtream Codes (`/timeshift/<user>/<pass>/<min>/<Y-m-d:H-M>/<id>`)
      is not supported yet: the server's time zone is unclear.
- [ ] Fullscreen-first navigation: channel list and group picker as overlays
      over the video, fully usable from the keyboard (and a remote or a
      gamepad, see "Gamepads and Steam Deck").
- [ ] Several playlists (providers) at once.
- [ ] Parental control: PIN for chosen groups/channels.
- [ ] Picture-in-picture (second player instance) — later.

## Interface

- [ ] Design pass: own font (e.g. Inter) and an icon font (e.g. Phosphor via
      `egui-phosphor`) instead of emoji-style symbols, spacing, a custom
      dark/light palette, hover/press animations.
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

## Testing

- Public playlists for manual tests (no keys, many formats and odd names):
  <https://github.com/iptv-org/iptv/tree/master/streams>. Never commit
  downloaded copies; unit tests keep using synthetic `example.com` data.

## Gamepads and Steam Deck

- [ ] Run on the Steam Deck (SteamOS: x86_64 Linux, AMD GPU, 1280×800).
      Desktop Mode: the Flatpak (libmpv bundled; SteamOS has a read-only
      system, so no `libmpv2` package). Game Mode: added as a non-Steam game, runs fullscreen under
      gamescope. Check VAAPI hardware decoding on the Deck's GPU.
- [ ] Gamepad control. Steam Input can already map the Deck's controls to
      keys (D-pad → PageUp/PageDown, A → Enter, B → Esc), so a keyboard-only
      UI is the first step (see "Fullscreen-first navigation"). Native support
      later, e.g. with the `gilrs` crate (reads controllers without SDL):
      D-pad moves through the list, A plays, B goes back, shoulder buttons
      switch groups, triggers change volume.
- [ ] "TV" layout for small screens / couch distance: larger text and rows,
      focus highlight that is easy to see.

## Packaging

- [ ] Check the Flatpak on Ubuntu (Wayland), KDE and the Steam Deck: video,
      VA-API decoding, file chooser, icon.
- [ ] Publish on Flathub (needs a submission and review; the manifest already
      builds offline).
- [ ] Maybe an AppImage as well (no runtime download).
- [ ] Windows installer (MSI) next to the zip: Start menu entry, uninstall
      from Settings → Apps, and a newer MSI replaces the old one in place
      (fixed `UpgradeCode`). `cargo-wix` (WiX toolset) builds it on the
      Windows CI runner; keep libmpv-2.dll beside the exe. Settings stay in
      %APPDATA% and survive upgrades. Later: a winget manifest, so
      `winget upgrade` updates it; code signing would stop SmartScreen
      warnings but costs money.
- [ ] macOS build, once someone can test it.
