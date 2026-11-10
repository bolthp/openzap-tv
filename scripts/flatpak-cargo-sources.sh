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
python="$work/venv/bin/python"
deps_ok() { "$python" -I -c 'import aiohttp, tomlkit, yaml' 2>/dev/null; }
if ! deps_ok; then
    rm -rf "$work/venv"
    python3 -m venv "$work/venv"
    # --isolated: ignore pip config and environment (some CI images make
    # pip install into the user's site-packages, outside the venv).
    "$python" -m pip install --isolated --quiet --disable-pip-version-check \
        'aiohttp>=3.9.5,<4' 'tomlkit>=0.13.3,<1' 'PyYAML>=6.0.2,<7'
    if ! deps_ok; then
        echo "cannot set up the Python modules for the generator:" >&2
        "$python" -I -c 'import aiohttp, tomlkit, yaml' || true
        exit 1
    fi
fi
"$python" -I "$generator" "$root/Cargo.lock" -o "$root/flatpak/cargo-sources.json"
echo "wrote flatpak/cargo-sources.json"
