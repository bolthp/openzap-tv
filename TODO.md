# TODO

Backlog, roughly in priority order. Move items to a commit message when done.

## Playback

- [ ] **Hardware decoding** (on by default, `auto-safe`, falls back to the
      CPU) works on Windows + NVIDIA (nvdec, also 4K HDR HEVC 10-bit),
      Windows + AMD integrated graphics (Radeon 610M: `d3d11va-copy`, 4K HDR
      at full frame rate) and the Steam Deck (VAAPI: `vaapi-copy`, 4K HEVC
      10-bit). Not yet tried: Windows with Intel graphics, Linux with NVIDIA.
      On Linux, VAAPI may need the X11 or Wayland display passed to mpv's
      renderer (not done yet) to decode without copying frames.

## Inspired by OTT-Play

Feature set to aim for (see the OTT-Play player for TVs); roughly by value.

- [ ] Archive: shift (`tvg-rec`) works with a real provider; timeshift is
      only tested against a local server so far. Flussonic and templates are only tested against synthetic
      URLs. Flussonic "watch from the start"
      of a running programme asks for its full length; check the server's
      answer. Timeshift keys (Space, arrows, Home, End) not yet tried on
      Windows. Xtream Codes (`/timeshift/<user>/<pass>/<min>/<Y-m-d:H-M>/<id>`)
      is not supported yet: the server's time zone is unclear.
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

- [ ] Possibly: audio/subtitle language preference, deinterlacing.

## Network

- [x] Proxy (settings → Network): HTTP and SOCKS4/5 for playlists, guides
      and logos (ureq); streams only through an HTTP proxy (mpv's
      `http-proxy`, FFmpeg has no SOCKS). Empty: ureq follows `ALL_PROXY` /
      `HTTPS_PROXY` / `HTTP_PROXY`. Not yet tried with a real proxy server.

## Logos

- [ ] Limit the logo textures kept in memory (LRU) for very long lists.

## Testing

- Public playlists for manual tests (no keys, many formats and odd names):
  <https://github.com/iptv-org/iptv/tree/master/streams>. Never commit
  downloaded copies; unit tests keep using synthetic `example.com` data.

## Gamepads and Steam Deck

- [x] Runs on the Steam Deck: the Flatpak in Desktop Mode, a non-Steam
      game in Game Mode (fullscreen under gamescope); the settings dialog
      fits the screen. Bluetooth headphones work (the sound loss seen in
      0.5.1 did not come back).
- [x] No sound in Game Mode (0.5.2): WirePlumber restored a saved mute for
      streams named "mpv". 0.5.3 names the stream "OpenZap", shows a mute
      by the system mixer and lifts it with M; sound confirmed on the Deck.
      Still to see there: the "Muted in the system mixer" note and M.
- [x] No way to quit in Game Mode except the Steam menu: quit button (🚪)
      (0.5.4, works on the Deck). `Ctrl` + `Q` cannot be typed there (no Ctrl
      on Steam's keyboard): a gamepad button for quitting comes with native
      gamepad support.
- [x] Browse… in Game Mode showed no dialog (no portal backend for it) and
      left processes behind that kept Steam thinking the game still ran.
      0.5.5 uses a built-in file browser there (`XDG_CURRENT_DESKTOP`
      contains gamescope, or `SteamGamepadUI=1`; the `session:` log line
      shows the variables). Works on the Deck: fits the screen, no leftover
      processes, Steam returns to the library at once; "All files" and moving
      through the list with the D-pad work too.
- [x] Gamepad control (0.6.0): `gilrs` on a thread, buttons become the
      existing keys (src/pad.rs). Works on the Deck with Steam's Gamepad
      template ("Steam Virtual Gamepad", SDL mapping), also when the layout
      is switched while running; all buttons, the file browser and
      View + Menu quit confirmed. Holding ↓ on the video opened 10 streams
      a second: switches in quick succession now play only where they stop.
- [x] Settings dialog with a gamepad (0.6.0, Deck): D-pad moves between
      fields, A toggles/opens, B cancels, the focus outline is visible.
- [x] Interface scale (settings, `Ctrl` + `+`/`-`/`0`, 75–200 %) works on
      the Deck; no automatic "TV" scale for Game Mode needed (75 % is
      preferred there).

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
