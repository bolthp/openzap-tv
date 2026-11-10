#!/usr/bin/env bash
# Builds the Flatpak and a single-file bundle:
#   scripts/flatpak-build.sh [openzap.flatpak]
# Needs flatpak and flatpak-builder; the runtime and SDK come from Flathub.
# Build files go to ~/.cache/openzap-flatpak (OPENZAP_FLATPAK_WORK).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work=${OPENZAP_FLATPAK_WORK:-$HOME/.cache/openzap-flatpak}
app=io.github.bolthp.OpenZap
bundle=${1:-$root/openzap.flatpak}

"$root/scripts/flatpak-cargo-sources.sh"
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --force-clean --ccache \
    --state-dir="$work/state" --repo="$work/repo" \
    "$work/build" "$root/flatpak/$app.yml"
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
    "$work/repo" "$bundle" "$app"
echo "bundle: $bundle"
echo "install: flatpak install --user $bundle"
