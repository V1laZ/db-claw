#!/usr/bin/env bash
# Package DB Claw for Linux as <out_dir>/<name>.AppImage (also used by
# in-app updates).
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
