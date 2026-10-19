#!/bin/sh
# Build Postmesh.app (double-click launchable, no terminal) into ./dist
set -e
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release
APP=dist/Postmesh.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/postmesh "$APP/Contents/MacOS/Postmesh"

# icon
ICONSET=$(mktemp -d)/icon.iconset
mkdir -p "$ICONSET"
for s in 16 32 64 128 256 512; do
  sips -z $s $s assets/icon.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s*2)) $((s*2)) assets/icon.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Postmesh</string>
  <key>CFBundleDisplayName</key><string>Postmesh</string>
  <key>CFBundleIdentifier</key><string>io.postmesh.app</string>
  <key>CFBundleExecutable</key><string>Postmesh</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>NSHumanReadableCopyright</key><string>Developed by Carlos Couto (ccouto@ua.pt)</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

# ad-hoc signature so macOS (Apple Silicon) runs it without complaints
codesign --force --deep -s - "$APP"
echo "Built $APP"
