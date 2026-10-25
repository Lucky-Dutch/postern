#!/bin/sh
# Packs Postern into a disk image that installs by dragging the app onto the
# Applications shortcut: dist/Postern-<version>-<architecture>.dmg.
#
# The app has an ad-hoc signature and is not notarized, so on another Mac the
# first launch must be allowed in System Settings > Privacy & Security.
set -eu
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
architecture=$(uname -m)
image="dist/Postern-$version-$architecture.dmg"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

sh scripts/bundle-macos.sh
staging=$(mktemp -d "${TMPDIR:-/tmp}/postern-dmg.XXXXXX")
trap 'rm -rf "$staging"' EXIT INT TERM
mkdir "$staging/content"
ditto dist/Postern.app "$staging/content/Postern.app"
ln -s /Applications "$staging/content/Applications"
# Keep only the image: a loose copy in dist/ would be listed by Spotlight.
"$lsregister" -u "$PWD/dist/Postern.app" >/dev/null 2>&1 || true
rm -rf dist/Postern.app

# Build a writable image first, so the volume can carry the Postern icon.
rm -f "$image"
hdiutil create -volname Postern -srcfolder "$staging/content" -fs HFS+ -format UDRW "$staging/rw.dmg" >/dev/null
mkdir "$staging/mount"
hdiutil attach "$staging/rw.dmg" -readwrite -noverify -noautoopen -nobrowse -mountpoint "$staging/mount" >/dev/null
if setfile=$(xcrun --find SetFile 2>/dev/null); then
  cp assets/Postern.icns "$staging/mount/.VolumeIcon.icns"
  "$setfile" -c icnC "$staging/mount/.VolumeIcon.icns"
  "$setfile" -a C "$staging/mount"
fi
hdiutil detach "$staging/mount" -quiet
hdiutil convert "$staging/rw.dmg" -format UDZO -imagekey zlib-level=9 -o "$image" >/dev/null
printf 'Built %s\n' "$image"
