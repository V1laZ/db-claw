#!/usr/bin/env bash
# Package DB Claw for Linux:
#   <out_dir>/<name>.AppImage   self-contained app, used by in-app updates
#   <out_dir>/<name>.tar.gz     plain binaries
#
#   scripts/package-linux.sh <binary dir> <out dir> <name>
# Needs appimagetool on PATH (or $APPIMAGETOOL).
set -euo pipefail
bin_dir=$1
out_dir=$2
name=$3
root="$(cd "$(dirname "$0")/.." && pwd)"
tool=${APPIMAGETOOL:-appimagetool}

mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# tar.gz
mkdir -p "$stage/$name"
cp "$bin_dir/dbclaw" "$bin_dir/dbclaw-ctl" "$root/README.md" "$root/LICENSE" "$stage/$name/"
tar -C "$stage" -czf "$out_dir/$name.tar.gz" "$name"

# AppImage
app="$stage/AppDir"
mkdir -p "$app/usr/bin" "$app/usr/share/applications" "$app/usr/share/icons/hicolor/512x512/apps"
cp "$bin_dir/dbclaw" "$bin_dir/dbclaw-ctl" "$app/usr/bin/"
cp "$root/packaging/linux/AppRun" "$app/AppRun"
cp "$root/packaging/linux/dbclaw.desktop" "$app/dbclaw.desktop"
cp "$root/packaging/linux/dbclaw.desktop" "$app/usr/share/applications/dbclaw.desktop"
cp "$root/assets/icon.png" "$app/dbclaw.png"
cp "$root/assets/icon.png" "$app/usr/share/icons/hicolor/512x512/apps/dbclaw.png"
ln -s dbclaw.png "$app/.DirIcon"
ARCH=x86_64 "$tool" --no-appstream "$app" "$out_dir/$name.AppImage"

echo "$out_dir/$name.AppImage"
echo "$out_dir/$name.tar.gz"
