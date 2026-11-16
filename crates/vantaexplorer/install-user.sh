#!/usr/bin/env bash
# User-local install, no root: puts vantaexplorer on PATH and registers its .desktop entry and
# icon, so launchers can find it and other apps can offer it for folders. With vantaportal built
# too, it also registers Vanta Explorer as a file chooser for xdg-desktop-portal.
#
# Build first:  cargo build --release -p vantaexplorer -p vantaportal
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
bin="$root/target/release/vantaexplorer"
if [ ! -x "$bin" ]; then
  echo "Build first:  cargo build --release -p vantaexplorer" >&2
  exit 1
fi

apps="$HOME/.local/share/applications"
icons="$HOME/.local/share/icons/hicolor"
mkdir -p "$HOME/.local/bin" "$apps"

# A copy, not a link: a link into the build tree dies with the next `cargo clean`, and
# replacing the directory entry leaves a running instance on its old inode.
cp -f "$bin" "$HOME/.local/bin/vantaexplorer.new"
mv -f "$HOME/.local/bin/vantaexplorer.new" "$HOME/.local/bin/vantaexplorer"

# Absolute Exec, so the entry works even when the launcher has a different PATH.
sed "s|^Exec=vantaexplorer|Exec=$HOME/.local/bin/vantaexplorer|" \
    "$here/vantaexplorer.desktop" > "$apps/vantaexplorer.desktop"

# The file chooser: the program that answers the portal, the D-Bus entry that starts it and the
# entry that tells the portal it exists. Choosing it is left to the user (see the last lines).
portal="$root/target/release/vantaportal"
if [ -x "$portal" ]; then
  cp -f "$portal" "$HOME/.local/bin/vantaportal.new"
  mv -f "$HOME/.local/bin/vantaportal.new" "$HOME/.local/bin/vantaportal"
  mkdir -p "$HOME/.local/share/dbus-1/services" "$HOME/.local/share/xdg-desktop-portal/portals"
  sed "s|^Exec=.*|Exec=$HOME/.local/bin/vantaportal|" \
      "$here/org.freedesktop.impl.portal.desktop.vanta.service" \
      > "$HOME/.local/share/dbus-1/services/org.freedesktop.impl.portal.desktop.vanta.service"
  cp -f "$here/vanta.portal" "$HOME/.local/share/xdg-desktop-portal/portals/vanta.portal"
  # "Show in folder" from other programs, only where it was switched on (docs/install.md): the
  # entry is then pointed at this vantaportal.
  fm="$HOME/.local/share/dbus-1/services/org.freedesktop.FileManager1.service"
  if [ -f "$fm" ] && grep -q vantaportal "$fm"; then
    sed "s|^Exec=[^ ]*|Exec=$HOME/.local/bin/vantaportal|" "$here/org.freedesktop.FileManager1.service" > "$fm"
  fi
  # The bus reads its service files again; a running chooser service makes way for the new one.
  busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig >/dev/null 2>&1 || true
  pkill -x vantaportal 2>/dev/null || true
fi

# The icon sizes live in assets/icons and are made by scripts/make-icons.sh.
for f in "$here"/assets/icons/vantaexplorer-*.png; do
  s="${f##*-}"; s="${s%.png}"
  d="$icons/${s}x${s}/apps"; mkdir -p "$d"
  cp -f "$f" "$d/vantaexplorer.png"
done
# An older install may have left a vector icon, which launchers would pick over the PNGs.
rm -f "$icons/scalable/apps/vantaexplorer.svg"

# Refresh caches, best effort: launchers vary in how eagerly they rescan.
command -v update-desktop-database >/dev/null && update-desktop-database "$apps" || true
command -v gtk-update-icon-cache    >/dev/null && gtk-update-icon-cache -f -t "$icons" 2>/dev/null || true
command -v kbuildsycoca6            >/dev/null && kbuildsycoca6 >/dev/null 2>&1 || true

echo "Installed. Search for 'Vanta Explorer' in your launcher."
echo "To make it the default folder handler:"
echo "  xdg-mime default vantaexplorer.desktop inode/directory"
if [ -x "$portal" ]; then
  echo "To make it the file chooser other programs get, add to ~/.config/xdg-desktop-portal/portals.conf"
  echo "under [preferred], then run: systemctl --user restart xdg-desktop-portal"
  echo "  org.freedesktop.impl.portal.FileChooser=vanta;gtk"
fi
