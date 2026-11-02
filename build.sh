#!/bin/zsh
# Builds DiskDiff.app into target/DiskDiff.app.
#
#   ./build.sh            release build
#   ./build.sh --debug    debug build (faster to compile, slower to run)
#   ./build.sh --open     build, then launch the app
#   ./build.sh --install  build, then copy it to /Applications (replacing an older copy)
#
# Flags combine, e.g. `./build.sh --install --open`.
#
# A real bundle matters on macOS: Full Disk Access is granted per app, and a
# bare binary launched from a terminal borrows the terminal's permissions.
set -euo pipefail
cd "$(dirname "$0")"

profile=release
open_app=false
install=false
for arg in "$@"; do
  case $arg in
    --debug) profile=debug ;;
    --open) open_app=true ;;
    --install) install=true ;;
    -h|--help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg (see --help)" >&2; exit 1 ;;
  esac
done

if [[ $(uname) != Darwin ]]; then
  # No bundle outside macOS; the binary is the app.
  if [[ $profile == release ]]; then cargo build --release -p diskdiff; else cargo build -p diskdiff; fi
  echo "built target/$profile/diskdiff"
  exit 0
fi

if [[ $profile == release ]]; then cargo build --release -p diskdiff; else cargo build -p diskdiff; fi

APP=target/DiskDiff.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$profile/diskdiff" "$APP/Contents/MacOS/DiskDiff"
cp app/icon/DiskDiff.icns "$APP/Contents/Resources/DiskDiff.icns"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>DiskDiff</string>
  <key>CFBundleDisplayName</key><string>DiskDiff</string>
  <key>CFBundleIdentifier</key><string>app.diskdiff.native</string>
  <key>CFBundleExecutable</key><string>DiskDiff</string>
  <key>CFBundleIconFile</key><string>DiskDiff</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <!-- Finder's right-click "Temporary Scan with DiskDiff"; handled in app/src/services.rs. -->
  <key>NSServices</key>
  <array>
    <dict>
      <key>NSMenuItem</key><dict><key>default</key><string>Temporary Scan with DiskDiff</string></dict>
      <key>NSMessage</key><string>temporaryScan</string>
      <key>NSPortName</key><string>DiskDiff</string>
      <key>NSRequiredContext</key><dict/>
      <key>NSSendFileTypes</key><array><string>public.folder</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
# Ad-hoc signature so macOS can attach privacy grants to a stable identity.
codesign --force --deep --sign - "$APP" >/dev/null 2>&1
# Make Finder and the Dock pick up a changed icon instead of a cached one.
touch "$APP"
echo "built $APP ($profile, v$VERSION)"

if $install; then
  dest=/Applications/DiskDiff.app
  # Never replace some other app that happens to use the name.
  if [[ -d $dest ]]; then
    existing=$(defaults read "$dest/Contents/Info.plist" CFBundleIdentifier 2>/dev/null || true)
    if [[ $existing != app.diskdiff.native ]]; then
      echo "$dest belongs to another app (${existing:-unknown}); not replacing it" >&2
      exit 1
    fi
  fi
  # Quit only this app, matched by its full path.
  pkill -f "^$dest/Contents/MacOS/DiskDiff" 2>/dev/null || true
  rm -rf "$dest"
  cp -R "$APP" /Applications/
  APP=/Applications/DiskDiff.app
  echo "installed $APP"
fi

if $open_app; then
  open "$APP"
fi
