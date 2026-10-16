#!/usr/bin/env bash
# Fetches and verifies the pinned Chrome for Testing build for Linux declared in
# crates/florui-conformance/chromium-pin.json, extracting it under
# .tools/chromium/<version>/ so `florui compare` finds it with no --chromium
# flag. Idempotent, and it refuses to extract an archive whose SHA-256 differs
# from the pin. Needs curl, sha256sum and unzip.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
pin="$root/crates/florui-conformance/chromium-pin.json"

version=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$pin" | head -1)
url=$(sed -n '/"linux64"/,/}/s/.*"url": *"\([^"]*\)".*/\1/p' "$pin")
sha256=$(sed -n '/"linux64"/,/}/s/.*"sha256": *"\([^"]*\)".*/\1/p' "$pin")
if [ -z "$version" ] || [ -z "$url" ] || [ -z "$sha256" ]; then
    echo "chromium-pin.json does not pin a Linux build" >&2
    exit 1
fi

dest="$root/.tools/chromium/$version"
exe="$dest/chrome-linux64/chrome"
if [ -x "$exe" ]; then
    echo "Chrome for Testing $version already present at $exe"
    exit 0
fi

mkdir -p "$dest"
zip="$dest/chrome-linux64.zip"
echo "Downloading Chrome for Testing $version (linux64) from $url ..."
curl -fsSL -o "$zip" "$url"

if ! echo "$sha256  $zip" | sha256sum -c - >/dev/null; then
    rm -f "$zip"
    echo "SHA-256 mismatch for Chrome for Testing $version: refusing to extract an unverified download." >&2
    exit 1
fi
echo "Checksum verified ($sha256)."

unzip -q -o "$zip" -d "$dest"
rm -f "$zip"
if [ ! -x "$exe" ]; then
    echo "Extraction succeeded but $exe was not produced: the archive layout may have changed." >&2
    exit 1
fi
echo "Chrome for Testing $version ready at $exe"
