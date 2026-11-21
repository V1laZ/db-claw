#!/usr/bin/env bash
# Package DB Claw as a macOS .app bundle inside a zip.
#   scripts/package-macos.sh <binary dir> <version> <output.zip>
# e.g. scripts/package-macos.sh target/aarch64-apple-darwin/release 0.1.0 dist/db-claw-macos.zip
set -euo pipefail
bin_dir=$1
version=$2
out=$3

stage=$(mktemp -d)
app="$stage/DB Claw.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin_dir/dbclaw" "$app/Contents/MacOS/dbclaw"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>DB Claw</string>
  <key>CFBundleDisplayName</key><string>DB Claw</string>
  <key>CFBundleIdentifier</key><string>dev.dbclaw.DBClaw</string>
  <key>CFBundleExecutable</key><string>dbclaw</string>
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
# launch — right-click → Open, or remove the quarantine attribute.
codesign --force --deep --sign - "$app"

mkdir -p "$stage/db-claw"
mv "$app" "$stage/db-claw/"
cp "$bin_dir/dbclaw-ctl" "$stage/db-claw/"
cp README.md "$stage/db-claw/"
mkdir -p "$(dirname "$out")"
out_abs="$(cd "$(dirname "$out")" && pwd)/$(basename "$out")"
(cd "$stage" && ditto -c -k --norsrc --noextattr --noqtn --keepParent db-claw "$out_abs")
rm -rf "$stage"
echo "$out_abs"
