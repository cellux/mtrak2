#!/bin/sh
# Replace the checked-in standalone Fennel compiler asset.
#
# Usage:
#   FENNEL_VERSION=1.6.1 scripts/update-fennel.sh
#
# Environment overrides (used by tests):
#   FENNEL_URL_BASE  base URL for the release distribution
#                    (default: https://fennel-lang.org/downloads)
#   FENNEL_ASSET     path of the checked-in asset to replace
#                    (default: crates/mtrak-engine/assets/fennel.lua)
#
# The asset is only replaced after a successful download, so a failed download
# leaves the existing compiler untouched.
set -eu

if [ -z "${FENNEL_VERSION:-}" ]; then
    echo "error: FENNEL_VERSION is required (e.g. make update-fennel FENNEL_VERSION=1.6.1)" >&2
    exit 1
fi

base="${FENNEL_URL_BASE:-https://fennel-lang.org/downloads}"
asset="${FENNEL_ASSET:-crates/mtrak-engine/assets/fennel.lua}"
url="$base/fennel-$FENNEL_VERSION.lua"

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

echo "downloading $url"
if ! curl -fsSL "$url" -o "$tmp"; then
    echo "error: failed to download $url" >&2
    exit 1
fi

asset_dir="$(dirname "$asset")"
mkdir -p "$asset_dir"
mv "$tmp" "$asset"
trap - EXIT
echo "updated $asset to Fennel $FENNEL_VERSION"
