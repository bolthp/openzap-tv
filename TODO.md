# TODO

Backlog, roughly in priority order. Move items to a commit message when done.

## Playback

- [ ] **Stream info on channel switch**: resolution, codec, frame rate, bitrate,
      HDR type, hardware/software decoding — briefly on the video, and on demand
      (e.g. `I` key). Lets you check whether a "4K" channel really is 4K.
- [ ] **Hardware decoding** (`hwdec`). Today everything, including 4K HDR, is
      decoded on the CPU. Needs testing per platform (Windows: d3d11va/nvdec,
      Linux: vaapi/nvdec) and a fallback when interop with our GL context fails.
- [ ] Give up on streams that never deliver a frame (e.g. 20 s in "Loading...")
      with a clear message instead of waiting forever.
- [ ] Check `PageUp` / `PageDown` channel switching on Windows (not tested yet).
- [ ] Scroll the channel list to the channel switched to with `PageUp`/`PageDown`.

## Programme guide

- [ ] Show times in the local time zone (needs a time-zone crate).
- [ ] Day schedule for a channel, not only now/next.
- [ ] Match all `display-name` variants of EPG channels (about half of the test
      playlist's channels currently find no EPG).
- [ ] Refresh the guide automatically (daily); data is kept for 4 days ahead.

## Interface

- [ ] Design pass over the whole UI (layout, spacing, typography, icons).
- [ ] File picker for playlist/EPG instead of typing paths.

## Network

- [ ] Proxy support (HTTP/SOCKS) for playlist, EPG and streams — mpv has
      `http-proxy`, ureq supports proxies. Today streams that need it only
      work through a system VPN.

## Packaging

- [ ] Linux AppImage bundling libmpv (today the system `libmpv2` is required).
- [ ] macOS build, once someone can test it.
