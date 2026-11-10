#!/usr/bin/env bash
# Writes flatpak/cargo-sources.json from Cargo.lock: Flatpak builds have no
# network, so every crate is listed for flatpak-builder to download first.
# Uses a pinned flatpak-cargo-generator in a private virtualenv.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work=${OPENZAP_FLATPAK_WORK:-$HOME/.cache/openzap-flatpak}
rev=41c20aa10819cdb2a4f3ca171758a96d1955c018
url="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$rev/cargo/flatpak-cargo-generator.py"

mkdir -p "$work"
generator="$work/flatpak-cargo-generator-$rev.py"
if [ ! -f "$generator" ]; then
    curl -sfL "$url" -o "$generator.part"
    mv "$generator.part" "$generator"
fi
if [ ! -x "$work/venv/bin/python" ]; then
    python3 -m venv "$work/venv"
    "$work/venv/bin/pip" install --quiet 'aiohttp>=3.9.5,<4' 'tomlkit>=0.13.3,<1' 'PyYAML>=6.0.2,<7'
fi
"$work/venv/bin/python" -I "$generator" "$root/Cargo.lock" -o "$root/flatpak/cargo-sources.json"
echo "wrote flatpak/cargo-sources.json"
