#!/bin/sh
# Builds dist/Postern.app. With --install, copies it to /Applications, or to
# ~/Applications when /Applications is not writable, and removes the copy in
# dist/, so Spotlight and the Apps view list Postern once.
set -eu
cd "$(dirname "$0")/.."
install=false
case "${1:-}" in
  --install) install=true ;;
  "") ;;
  *) printf 'usage: %s [--install]\n' "$0" >&2; exit 2 ;;
esac
CARGO=${CARGO:-cargo}
"$CARGO" build --release --locked
bundle="dist/Postern.app"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp target/release/postern "$bundle/Contents/MacOS/postern"
cp assets/Postern.icns "$bundle/Contents/Resources/Postern.icns"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>postern</string>
  <key>CFBundleIdentifier</key><string>dev.postern.app</string>
  <key>CFBundleName</key><string>Postern</string>
  <key>CFBundleDisplayName</key><string>Postern</string>
  <key>CFBundleIconFile</key><string>Postern</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>Copyright © 2026 Lucky-Dutch. MIT License.</string>
</dict></plist>
PLIST
codesign --force --sign - "$bundle"
printf 'Built %s\n' "$bundle"
if [ "$install" = true ]; then
  lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
  applications=/Applications
  if [ ! -w "$applications" ]; then
    applications="$HOME/Applications"
    mkdir -p "$applications"
  fi
  ditto "$bundle" "$applications/Postern.app"
  "$lsregister" -u "$PWD/$bundle" >/dev/null 2>&1 || true
  rm -rf "$bundle"
  "$lsregister" -f "$applications/Postern.app"
  printf 'Installed %s/Postern.app\n' "$applications"
fi
