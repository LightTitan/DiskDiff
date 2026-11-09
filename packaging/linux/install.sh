#!/bin/sh
# Installs DiskDiff for the current user (no root needed):
#   ~/.local/bin/diskdiff, ~/.local/bin/diskdiff-mcp,
#   a menu entry and its icon under ~/.local/share.
# Run it from the unpacked folder: ./install.sh   (./install.sh --uninstall to remove)
set -eu
cd "$(dirname "$0")"
bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$bin/diskdiff" "$bin/diskdiff-mcp" "$data/applications/diskdiff.desktop" \
    "$data/icons/hicolor/512x512/apps/diskdiff.png"
  echo "DiskDiff removed (your scans in $data/diskdiff are kept)."
  exit 0
fi
mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/512x512/apps"
install -m 755 diskdiff diskdiff-mcp "$bin/"
install -m 644 diskdiff.png "$data/icons/hicolor/512x512/apps/diskdiff.png"
# Absolute Exec path, so the menu entry works even if ~/.local/bin isn't on PATH.
sed "s|^Exec=diskdiff$|Exec=$bin/diskdiff|" diskdiff.desktop > "$data/applications/diskdiff.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$data/applications" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q "$data/icons/hicolor" || true
echo "DiskDiff installed: $bin/diskdiff (and a menu entry)."
case ":$PATH:" in *":$bin:"*) ;; *) echo "Note: $bin is not on your PATH." ;; esac
