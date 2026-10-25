#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
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
