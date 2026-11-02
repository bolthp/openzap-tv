# CLAUDE.md

## Stack

- Rust stable
- GUI: egui
- Playback: libmpv (dynamically linked, shipped with the app)

## Platforms

- Targets: Windows and Linux (primary), macOS (secondary, tested by others or in a VM).
- WSL is only a development/testing environment, not a target. WSL-specific workarounds (`platform.rs`, `platform_options` in `player.rs`) must stay isolated, small and clearly marked; never let them shape the design for real platforms.

## Rules

- Prefer `std`. Add external crates only when necessary, and explain why.
- Never commit real playlists, stream URLs with access keys, or EPG dumps. Test data must be synthetic (`example.com`, fake keys).
- Mask access keys in stream URLs in all logs.
- Every new feature comes with unit tests. Before committing run `cargo fmt`, `cargo clippy`, `cargo test`.
- The author is new to Rust: in chat replies, briefly explain non-obvious concepts (ownership, lifetimes, traits). Keep code comments concise.
- Code, comments, docs and commit messages in English.
