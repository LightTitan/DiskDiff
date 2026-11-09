#!/bin/zsh
# Builds DiskDiff.app into target/DiskDiff.app, with the AI agent tool
# (diskdiff-cli) inside it at Contents/MacOS/diskdiff-cli.
#
#   ./build.sh            release build
#   ./build.sh --debug    debug build (faster to compile, slower to run)
#   ./build.sh --open     build, then launch the app
#   ./build.sh --install  build, then copy it to /Applications (replacing an older copy)
#   ./build.sh --universal  one binary for Apple silicon and Intel, as releases ship
#   ./build.sh --dmg      also package the app as target/DiskDiff_<version>_<arch>.dmg
#
# Flags combine, e.g. `./build.sh --install --open` or `./build.sh --universal --dmg`.
# Elsewhere (Linux with zsh installed) it only builds the two binaries,
# diskdiff and diskdiff-cli, into target/<profile>/; on Windows run
# `cargo build --release -p diskdiff -p diskdiff-cli`. CI packages both.
#
# A real bundle matters on macOS: Full Disk Access is granted per app, and a
# bare binary launched from a terminal borrows the terminal's permissions.
set -euo pipefail
cd "$(dirname "$0")"

profile=release
open_app=false
install=false
universal=false
dmg=false
for arg in "$@"; do
  case $arg in
    --debug) profile=debug ;;
    --open) open_app=true ;;
    --install) install=true ;;
    --universal) universal=true ;;
    --dmg) dmg=true ;;
    -h|--help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg (see --help)" >&2; exit 1 ;;
  esac
done

bins=(diskdiff diskdiff-cli)
cargo_flags=(-p diskdiff -p diskdiff-cli)
[[ $profile == release ]] && cargo_flags+=(--release)

if [[ $(uname) != Darwin ]]; then
  # No bundle outside macOS; the binaries are the app.
  cargo build $cargo_flags
  echo "built target/$profile/diskdiff and target/$profile/diskdiff-cli"
  exit 0
fi

if $universal; then
  targets=(aarch64-apple-darwin x86_64-apple-darwin)
  for t in $targets; do
    if [[ ! -d $(rustc --print target-libdir --target $t) ]]; then
      echo "no Rust standard library for $t (with rustup: rustup target add $t)" >&2
      exit 1
    fi
  done
  for t in $targets; do cargo build $cargo_flags --target $t; done
  out=target/universal/$profile
  mkdir -p "$out"
  for b in $bins; do
    lipo -create -output "$out/$b" target/${^targets}/$profile/$b
  done
  arch=universal
else
  cargo build $cargo_flags
  out=target/$profile
  arch=$(uname -m)
fi

APP=target/DiskDiff.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$out/diskdiff" "$APP/Contents/MacOS/DiskDiff"
# Next to the app's own binary, so agents can be pointed at one stable path
# inside the installed app.
cp "$out/diskdiff-cli" "$APP/Contents/MacOS/diskdiff-cli"
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
echo "built $APP ($profile, $arch, v$VERSION)"

if $dmg; then
  # The usual drag-to-install window: the app next to a link to /Applications.
  DMG=target/DiskDiff_${VERSION}_${arch}.dmg
  stage=$(mktemp -d)
  ditto "$APP" "$stage/DiskDiff.app"
  ln -s /Applications "$stage/Applications"
  rm -f "$DMG"
  hdiutil create -quiet -volname DiskDiff -srcfolder "$stage" -format UDZO -ov "$DMG"
  rm -rf "$stage"
  echo "packaged $DMG"
fi

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
