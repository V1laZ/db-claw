#!/usr/bin/env bash
# Package DB Claw for macOS:
#   <out_dir>/<name>.dmg          drag-to-Applications installer
#   <out_dir>/<name>.app.tar.gz   the bare .app, used by in-app updates
#
#   scripts/package-macos.sh <binary dir> <version> <out dir> <name>
# e.g. scripts/package-macos.sh target/aarch64-apple-darwin/release 0.2.0 dist db-claw-v0.2.0-macos-arm64
set -euo pipefail
bin_dir=$1
version=$2
out_dir=$3
name=$4
root="$(cd "$(dirname "$0")/.." && pwd)"

mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

app="$stage/DB Claw.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin_dir/dbclaw" "$app/Contents/MacOS/dbclaw"
# The automation CLI ships inside the bundle; README explains the symlink.
cp "$bin_dir/dbclaw-ctl" "$app/Contents/MacOS/dbclaw-ctl"
cp "$root/assets/icon.icns" "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>DB Claw</string>
  <key>CFBundleDisplayName</key><string>DB Claw</string>
  <key>CFBundleIdentifier</key><string>dev.dbclaw.DBClaw</string>
  <key>CFBundleExecutable</key><string>dbclaw</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
# Ad-hoc signature (no Apple Developer ID): Gatekeeper still asks on first
# launch of a downloaded copy — right-click → Open. In-app updates aren't
# quarantined, so they open without asking.
codesign --force --deep --sign - "$app"

# Update artifact: just the bundle, without AppleDouble/xattr files.
COPYFILE_DISABLE=1 tar -C "$stage" --no-xattrs -czf "$out_dir/$name.app.tar.gz" "DB Claw.app" 2>/dev/null \
  || COPYFILE_DISABLE=1 tar -C "$stage" -czf "$out_dir/$name.app.tar.gz" "DB Claw.app"

# Installer disk image: background, app on the left, Applications on the
# right (layout in packaging/macos/). dmgbuild writes the Finder layout
# directly, so it works headless on CI.
dmgbuild=${DMGBUILD:-}
if [[ -z "$dmgbuild" ]]; then
  python3 -m venv "$stage/venv"
  "$stage/venv/bin/pip" install -q --disable-pip-version-check "dmgbuild==1.6.7"
  dmgbuild="$stage/venv/bin/dmgbuild"
fi
rm -f "$out_dir/$name.dmg"
"$dmgbuild" -s "$root/packaging/macos/dmg-settings.py" -D app="$app" -D root="$root" "DB Claw" "$out_dir/$name.dmg" >/dev/null

echo "$out_dir/$name.dmg"
echo "$out_dir/$name.app.tar.gz"
