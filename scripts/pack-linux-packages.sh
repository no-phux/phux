#!/usr/bin/env bash
# Repackage the verified Linux release tarballs as phux .deb and .rpm
# packages, beside them in DIST, with sha256 sidecars. The bytes inside are
# the tarball's, so a package and a curl | sh install run the same binary.
#
#   bash scripts/pack-linux-packages.sh vX.Y.Z <dist>
#
# With PHUX_PACKAGES_GPG_KEY set (an armored secret signing key), the .rpm
# files are signed for dnf's gpgcheck. apt checks the signed repository
# index instead (scripts/build-package-repo.sh), so a .deb is not signed.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

NFPM_VERSION=2.48.0
declare -A NFPM_SHA256=(
  [x86_64]=6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750
  [arm64]=a9a75213763783b7669719e505474c748c5ad7dc66591a195619deb50f85c2f4
)

die() {
  phux_error "$*"
  exit 1
}

[[ $# -eq 2 && $1 =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] || die "usage: pack-linux-packages.sh vX.Y.Z <dist>"
tag="$1"
version="${BASH_REMATCH[1]}"
dist="$2"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

nfpm="$(command -v nfpm || true)"
if [[ -z $nfpm ]]; then
  host="$(uname -m)"
  [[ $host == aarch64 ]] && host=arm64
  archive="nfpm_${NFPM_VERSION}_Linux_${host}.tar.gz"
  curl -fsSL --retry 3 -o "$work/$archive" \
    "https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}/${archive}"
  echo "${NFPM_SHA256[$host]:?no nfpm digest for $host}  $work/$archive" | sha256sum -c --quiet -
  tar -xzf "$work/$archive" -C "$work" nfpm
  nfpm="$work/nfpm"
fi

if [[ -n ${PHUX_PACKAGES_GPG_KEY:-} ]]; then
  command -v rpmsign >/dev/null || die "rpmsign is required to sign the .rpm packages (apt install rpm)"
  export GNUPGHOME="$work/gnupg"
  mkdir -m 700 "$GNUPGHOME"
  printf '%s\n' "$PHUX_PACKAGES_GPG_KEY" | gpg --batch --quiet --import
  # gpg picks the key's signing subkey; CI holds only that subkey.
  signer="$(gpg --list-secret-keys --with-colons | awk -F: '$1 == "fpr" { print $10; exit }')"
fi

# The man page is the same for both arches: render it with the binary this
# host can run. A release older than `phux man` ships without one.
man_entry=""
host_stem="phux-${tag}-$(uname -m)-unknown-linux-gnu"
if [[ -f "$dist/$host_stem.tar.gz" ]]; then
  tar -xzf "$dist/$host_stem.tar.gz" -C "$work"
  if "$work/$host_stem/phux" man >"$work/phux.1" 2>/dev/null; then
    man_entry="  - { src: $work/phux.1, dst: /usr/share/man/man1/phux.1 }"
  fi
fi
[[ -n $man_entry ]] || phux_warn "no man page: $host_stem could not render one"

for pair in x86_64:amd64 aarch64:arm64; do
  target="${pair%%:*}-unknown-linux-gnu"
  arch="${pair#*:}"
  stem="phux-${tag}-${target}"
  [[ -f "$dist/$stem.tar.gz" ]] || die "$dist/$stem.tar.gz is missing"
  tar -xzf "$dist/$stem.tar.gz" -C "$work"
  # The tarball's binaries need glibc 2.35 (scripts/check-binary-portability.sh).
  cat >"$work/nfpm.yaml" <<EOF
name: phux
arch: $arch
version: $version
release: 1
maintainer: phux <https://github.com/no-phux/phux/issues>
description: A terminal multiplexer whose server you can drive from code.
homepage: https://phux.sh
license: Apache-2.0
section: utils
priority: optional
contents:
  - { src: $work/$stem/phux, dst: /usr/bin/phux, file_info: { mode: 0755 } }
  - { src: $work/$stem/phux-mcp, dst: /usr/bin/phux-mcp, file_info: { mode: 0755 } }
  - { src: $work/$stem/README.md, dst: /usr/share/doc/phux/README.md }
  - { src: $work/$stem/LICENSE, dst: /usr/share/doc/phux/LICENSE }
  - { src: $work/$stem/NOTICE, dst: /usr/share/doc/phux/NOTICE }
  - { src: $work/$stem/THIRD-PARTY-NOTICES.md, dst: /usr/share/doc/phux/THIRD-PARTY-NOTICES.md }
$man_entry
overrides:
  deb: { depends: ["libc6 (>= 2.35)"] }
  rpm: { depends: ["glibc >= 2.35"] }
EOF
  deb="phux_${version}_${arch}.deb"
  "$nfpm" package --config "$work/nfpm.yaml" --packager deb --target "$dist/$deb" >/dev/null
  rpm_arch="${pair%%:*}"
  rpm="phux-${version}-1.${rpm_arch}.rpm"
  "$nfpm" package --config "$work/nfpm.yaml" --packager rpm --target "$dist/$rpm" >/dev/null
  if [[ -n ${signer:-} ]]; then
    rpmsign --define "__gpg $(command -v gpg)" --define "_gpg_name $signer" --addsign "$dist/$rpm" >/dev/null
  fi
  for package in "$deb" "$rpm"; do
    (cd "$dist" && sha256sum "$package" >"$package.sha256")
    phux_info "packaged $package"
  done
done
