#!/usr/bin/env bash
# Write the AUR phux-bin PKGBUILD and .SRCINFO for a release into OUT: the
# release tarballs, their digests pinned from the .sha256 sidecars in DIST.
#
#   bash scripts/aur-pkgbuild.sh vX.Y.Z <dist> <out>
#
# packages.yml pushes OUT to aur.archlinux.org when AUR_SSH_PRIVATE_KEY is set.
# .SRCINFO is what `makepkg --printsrcinfo` prints for the PKGBUILD;
# scripts/test-linux-packages.sh checks that, and builds it, in an Arch container.
set -euo pipefail

[[ $# -eq 3 && $1 =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] || {
  echo "usage: aur-pkgbuild.sh vX.Y.Z <dist> <out>" >&2
  exit 2
}
tag="$1"
version="${BASH_REMATCH[1]}"
url="https://github.com/no-phux/phux/releases/download/$tag"
digest() {
  local sum
  sum="$(awk '{ print $1; exit }' "$2/phux-$tag-$1-unknown-linux-gnu.tar.gz.sha256")"
  [[ $sum =~ ^[0-9a-f]{64}$ ]] || { echo "no sha256 for $1 in $2" >&2; exit 1; }
  printf '%s\n' "$sum"
}
x86_64="$(digest x86_64 "$2")"
aarch64="$(digest aarch64 "$2")"
desc='A terminal multiplexer whose server you can drive from code'
mkdir -p "$3"

cat >"$3/PKGBUILD" <<EOF
# Maintainer: phux <https://github.com/no-phux/phux>
# Written by scripts/aur-pkgbuild.sh in no-phux/phux for each release.
pkgname=phux-bin
pkgver=$version
pkgrel=1
pkgdesc='$desc'
arch=('x86_64' 'aarch64')
url='https://phux.sh'
license=('Apache-2.0')
depends=('glibc')
provides=('phux')
conflicts=('phux')
options=('!strip' '!debug')
source_x86_64=("$url/phux-$tag-x86_64-unknown-linux-gnu.tar.gz")
source_aarch64=("$url/phux-$tag-aarch64-unknown-linux-gnu.tar.gz")
sha256sums_x86_64=('$x86_64')
sha256sums_aarch64=('$aarch64')

package() {
  cd "phux-v\${pkgver}-\${CARCH}-unknown-linux-gnu"
  install -Dm755 phux phux-mcp -t "\$pkgdir/usr/bin"
  install -Dm644 LICENSE NOTICE -t "\$pkgdir/usr/share/licenses/\$pkgname"
  install -Dm644 README.md THIRD-PARTY-NOTICES.md -t "\$pkgdir/usr/share/doc/\$pkgname"
}
EOF

cat >"$3/.SRCINFO" <<EOF
pkgbase = phux-bin
	pkgdesc = $desc
	pkgver = $version
	pkgrel = 1
	url = https://phux.sh
	arch = x86_64
	arch = aarch64
	license = Apache-2.0
	depends = glibc
	provides = phux
	conflicts = phux
	options = !strip
	options = !debug
	source_x86_64 = $url/phux-$tag-x86_64-unknown-linux-gnu.tar.gz
	sha256sums_x86_64 = $x86_64
	source_aarch64 = $url/phux-$tag-aarch64-unknown-linux-gnu.tar.gz
	sha256sums_aarch64 = $aarch64

pkgname = phux-bin
EOF
