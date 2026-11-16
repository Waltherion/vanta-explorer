# Maintainer: Waltherion (https://github.com/Waltherion)

# Vanta Explorer on its own, built from the latest commit of its repository.
# Install: clone the repository, then `makepkg -si` in it. Update: `./update.sh` (see the README).
pkgname=vantaexplorer-git
pkgver=1.0.0
pkgrel=1
pkgdesc="File manager for Hyprland with true HDR previews"
arch=(x86_64)
url="https://github.com/Waltherion/vanta-explorer"
license=(GPL-3.0-or-later LGPL-3.0-or-later)
# The release profile keeps line tables for backtraces; a debug package is not worth building.
options=(!debug)
depends=(
  vulkan-icd-loader
  libxkbcommon
  fontconfig
  hicolor-icon-theme
  shared-mime-info
  xdg-utils
  file
  libarchive
  ffmpeg
  libavif
  libjxl
  libheif
  lcms2
  libraw
  poppler-glib
  cairo
  glib2
)
makedepends=(git cargo shaderc clang pkgconf)
optdepends=(
  'hyprland: HDR output, and matching the desktop SDR brightness'
  'udisks2: mounting and unmounting drives from the sidebar'
  'ttf-nerd-fonts-symbols: the icons in the toolbar and sidebar, when the font in use has none'
  'xdg-desktop-portal: be the file chooser other programs get (see docs/install.md)'
)
provides=(vantaexplorer)
conflicts=(vantaexplorer)
source=("$pkgname::git+$url.git")
sha256sums=('SKIP')

pkgver() {
  cd "$pkgname"
  printf "1.0.0.r%s.g%s" "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

prepare() {
  cd "$pkgname"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$pkgname"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  cargo build --frozen --release --package=vantaexplorer --package=vantaportal
}

check() {
  cd "$pkgname"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  cargo test --frozen --release --workspace
}

package() {
  cd "$pkgname"
  install -Dm755 target/release/vantaexplorer "$pkgdir/usr/bin/vantaexplorer"
  install -Dm644 crates/vantaexplorer/vantaexplorer.desktop \
    "$pkgdir/usr/share/applications/vantaexplorer.desktop"
  local f size
  for f in crates/vantaexplorer/assets/icons/vantaexplorer-*.png; do
    size=${f##*-}
    size=${size%.png}
    install -Dm644 "$f" "$pkgdir/usr/share/icons/hicolor/${size}x${size}/apps/vantaexplorer.png"
  done

  # The file chooser: the program that answers the portal, the entry that tells the portal it
  # exists, and the one that lets D-Bus start it. It is only used once chosen in portals.conf.
  install -Dm755 target/release/vantaportal "$pkgdir/usr/bin/vantaportal"
  install -Dm644 crates/vantaexplorer/vanta.portal \
    "$pkgdir/usr/share/xdg-desktop-portal/portals/vanta.portal"
  install -Dm644 crates/vantaexplorer/org.freedesktop.impl.portal.desktop.vanta.service \
    "$pkgdir/usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.vanta.service"
  # "Show in folder" from other programs. Not installed where D-Bus looks: Dolphin and Nautilus
  # answer to the same name, so it is copied into place by whoever wants it (docs/install.md).
  install -Dm644 crates/vantaexplorer/org.freedesktop.FileManager1.service \
    -t "$pkgdir/usr/share/vantaexplorer"

  # Its own icons: the folders, file types and drives of KDE's Breeze (LGPL-3.0-or-later, see
  # the README there), as an icon theme of their own. -d keeps the links between them.
  cp -dr --no-preserve=ownership crates/vantaexplorer/assets/icon-theme/vantaexplorer \
    "$pkgdir/usr/share/icons/"

  install -Dm644 README.md docs/config.md docs/install.md docs/omarchy.md \
    -t "$pkgdir/usr/share/doc/vantaexplorer"
  install -Dm644 omarchy/vanta-colors.jsonc.tpl -t "$pkgdir/usr/share/doc/vantaexplorer/omarchy"
}
