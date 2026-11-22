#!/usr/bin/env bash
# Re-render packaging/macos/dmg-background.tiff (1x + 2x) from the HTML source.
# Needs Google Chrome; the TIFF is committed so CI doesn't.
set -euo pipefail
dir="$(cd "$(dirname "$0")/../packaging/macos" && pwd)"
chrome=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
for scale in 1 2; do
  "$chrome" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=$scale \
    --window-size=660,400 --screenshot="$tmp/bg@$scale.png" "file://$dir/dmg-background.html" 2>/dev/null
done
tiffutil -cathidpicheck "$tmp/bg@1.png" "$tmp/bg@2.png" -out "$dir/dmg-background.tiff" 2>/dev/null
echo "$dir/dmg-background.tiff"
