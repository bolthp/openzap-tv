#!/usr/bin/env bash
# Prints the release name for CI: the tag (vX.Y.Z) when building a tag,
# else "vX.Y.Z-dryrun". For a tag, fails unless Cargo.toml has the same
# version and the Flatpak metainfo lists it.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
if [ "${GITHUB_REF_TYPE:-}" != "tag" ]; then
    echo "v$version-dryrun"
    exit 0
fi
if [ "v$version" != "$GITHUB_REF_NAME" ]; then
    echo "Tag $GITHUB_REF_NAME does not match Cargo.toml version $version" >&2
    exit 1
fi
if ! grep -q "<release version=\"$version\"" "$root/flatpak/io.github.bolthp.OpenZap.metainfo.xml"; then
    echo "flatpak/io.github.bolthp.OpenZap.metainfo.xml has no <release> for $version" >&2
    exit 1
fi
echo "$GITHUB_REF_NAME"
