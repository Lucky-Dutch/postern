#!/bin/sh
# Regenerates the icon, the README banners, and the framed README screenshots in
# assets/ from the Swift drawings in scripts/. The generated files are kept in
# the repository, so building does not need Swift. Run the smoke harness first
# to refresh docs/screenshots.
set -eu
cd "$(dirname "$0")/.."
work=$(mktemp -d "${TMPDIR:-/tmp}/postern-artwork.XXXXXX")
trap 'rm -rf "$work"' EXIT INT TERM
mkdir -p assets
swift scripts/icon.swift "$work/icon-1024.png" >/dev/null
iconset="$work/Postern.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$work/icon-1024.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$work/icon-1024.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o assets/Postern.icns
sips -z 512 512 "$work/icon-1024.png" --out assets/icon-512.png >/dev/null
for theme in dark light; do
  swift scripts/banner.swift "$theme" "$work/icon-1024.png" "assets/banner-$theme.png" >/dev/null
done
if [ -f docs/screenshots/table.png ] && [ -f docs/screenshots/table_light.png ]; then
  swift scripts/frame.swift dark docs/screenshots/table.png "$work/hero-dark.png" >/dev/null
  swift scripts/frame.swift light docs/screenshots/table_light.png "$work/hero-light.png" >/dev/null
  # Half size keeps the files small and still sharp at the README's width.
  for theme in dark light; do
    width=$(sips -g pixelWidth "$work/hero-$theme.png" | awk '/pixelWidth/ { print int($2 / 2) }')
    sips --resampleWidth "$width" "$work/hero-$theme.png" --out "assets/hero-$theme.png" >/dev/null
  done
fi
printf 'Wrote the icon, banners, and framed screenshots in assets/\n'
