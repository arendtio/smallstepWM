# Maintainer: smallstepwm
pkgname=smallstepwm
pkgver=0.1.0
pkgrel=6
pkgdesc='Small Wayland shell'
arch=('x86_64')
url='https://github.com/arendtio/smallstepWM'
license=('GPL-3.0-or-later')
depends=(
  'seatd'
  'libinput'
  'libxkbcommon'
  'pixman'
  'mesa'
  'libdrm'
  'wayland'
  'firefox'
  'alsa-utils'
  'ca-certificates'
)
makedepends=('rust' 'cargo' 'pkgconf' 'clang')
options=('!lto' '!debug')

build() {
  cd "$startdir"
  export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
  cargo build --release --locked
}

package() {
  cd "$startdir"
  install -Dm755 target/release/smallstepwm "$pkgdir/usr/bin/smallstepwm"
  install -Dm755 packaging/smallstep-session "$pkgdir/usr/bin/smallstep-session"
  install -Dm644 packaging/smallstep.desktop "$pkgdir/usr/share/wayland-sessions/smallstep.desktop"
}
