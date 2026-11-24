#!/usr/bin/env bash
# Rehearse the Linux package repositories end to end, in containers, with a
# throwaway key: package two real releases, serve the older one, install it
# with scripts/install.sh on fresh Ubuntu and Fedora, publish the newer one,
# and check `apt-get upgrade` and `dnf upgrade` pick it up. Then build and
# install the AUR phux-bin PKGBUILD for the newer one on Arch.
#
#   bash scripts/test-linux-packages.sh [<old tag> <new tag>]
#
# Needs docker (or podman as docker) and gh. Not a CI lane: it pulls images
# and two releases. docs/releasing.md#linux-packages says when to run it.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
old="${1:-v0.148.0}"
new="${2:-v0.149.0}"
TMP="$(mktemp -d)"
net="phux-pkg-$$"
cleanup() {
  docker rm -f "$net-pkgs" "$net-ubuntu" "$net-fedora" >/dev/null 2>&1 || true
  docker network rm "$net" >/dev/null 2>&1 || true
  rm -rf "$TMP"
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

mkdir -p "$TMP/dist"
for tag in "$old" "$new"; do
  gh release download "$tag" --repo no-phux/phux --dir "$TMP/dist" --clobber \
    --pattern "phux-$tag-*-unknown-linux-gnu.tar.gz*"
done

# Nothing here runs the installer on this host, but nothing may touch its HOME.
export DOCKER_CONFIG="${DOCKER_CONFIG:-$HOME/.docker}"
export HOME="$TMP/home"
docker network create "$net" >/dev/null
docker run -d --name "$net-pkgs" --network "$net" --network-alias pkgs -v "$TMP:/w" \
  python:3.13-alpine python -m http.server -d /w/site 8000 >/dev/null

# The builder makes a throwaway key shaped like the real one (a certify-only
# primary and a signing subkey, CI holding only the subkey), the packages,
# and the site, through the same scripts release.yml runs.
builder() {
  docker run --rm -v "$TMP:/w" -v "$ROOT:/src:ro" -e "STEP=$1" -e "OLD=$old" -e "NEW=$new" ubuntu:24.04 bash -euc '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq && apt-get install -y -qq apt-utils createrepo-c gnupg rpm curl ca-certificates >/dev/null
    export GNUPGHOME=/w/gnupg
    if [ ! -d "$GNUPGHOME" ]; then
      mkdir -m 700 "$GNUPGHOME"
      gpg --batch --quiet --passphrase "" --quick-gen-key "phux test key" rsa3072 cert never
      fpr=$(gpg --list-keys --with-colons | awk -F: "\$1 == \"fpr\" { print \$10; exit }")
      gpg --batch --quiet --passphrase "" --quick-add-key "$fpr" rsa3072 sign never
      gpg --armor --export > /w/test.asc
      sub=$(gpg --list-keys --with-colons | awk -F: "\$1 == \"fpr\" { n++ } n == 2 { print \$10; exit }")
      gpg --batch --armor --export-secret-subkeys "$sub!" > /w/test-secret.asc
    fi
    export PHUX_PACKAGES_GPG_KEY="$(cat /w/test-secret.asc)" PHUX_PACKAGES_PUBLIC_KEY=/w/test.asc
    export PHUX_PACKAGES_URL=http://pkgs:8000
    for tag in $OLD $([ "$STEP" = new ] && echo $NEW); do
      [ -d /w/pkgs/$tag ] && continue
      mkdir -p /w/pkgs/$tag && cp /w/dist/phux-$tag-*.tar.gz /w/pkgs/$tag/
      bash /src/scripts/pack-linux-packages.sh $tag /w/pkgs/$tag >/dev/null
      rm /w/pkgs/$tag/*.tar.gz /w/pkgs/$tag/*.sha256
    done
    rpm --import /w/test.asc && rpm -K /w/pkgs/*/*.rpm | grep -v "digests signatures OK" && exit 1
    bash /src/scripts/build-package-repo.sh /w/site /w/pkgs
    ls /w/site/repodata
  '
}
builder old
# install.sh with the throwaway key in place of packaging/phux.asc.
awk -v keyfile="$TMP/test.asc" '
  /^  cat <<.KEY.$/ { print; while ((getline line < keyfile) > 0) print line; skip = 1; next }
  skip && /^KEY$/ { skip = 0 }
  !skip { print }
' "$ROOT/scripts/install.sh" >"$TMP/install.sh"

want_old="${old#v}" want_new="${new#v}"
docker run -d --name "$net-ubuntu" --network "$net" -v "$TMP:/w:ro" ubuntu:22.04 sleep infinity >/dev/null
docker run -d --name "$net-fedora" --network "$net" -v "$TMP:/w:ro" fedora:43 sleep infinity >/dev/null
on() { docker exec -e PHUX_REPO_URL=http://pkgs:8000 -e PHUX_NO_SERVICE=1 "$net-$1" sh -c "$2"; }
on ubuntu 'sh /w/install.sh' || fail "install.sh on Ubuntu"
on fedora 'sh /w/install.sh' || fail "install.sh on Fedora"
on ubuntu 'dpkg -S /usr/bin/phux' | grep -q '^phux:' || fail "apt did not install /usr/bin/phux"
for host in ubuntu fedora; do
  on "$host" 'phux --version' | grep -q " $want_old" || fail "$host did not install $old"
done

builder new
on ubuntu 'apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get upgrade -y -qq' >/dev/null || fail "apt upgrade"
# The hour metadata_expire allows, passed.
on fedora 'dnf clean expire-cache -q && dnf upgrade -y -q' >/dev/null || fail "dnf upgrade"
for host in ubuntu fedora; do
  on "$host" 'phux --version' | grep -q " $want_new" || fail "$host did not upgrade to $new"
  on "$host" 'phux-mcp --help >/dev/null' || fail "$host phux-mcp does not run"
done

# makepkg refuses root, and Arch publishes x86_64 images only.
bash "$ROOT/scripts/aur-pkgbuild.sh" "$new" "$TMP/dist" "$TMP/aur"
docker run --rm --platform linux/amd64 -v "$TMP/aur:/aur:ro" archlinux:base-devel bash -euc '
  useradd -m builder && cp -r /aur /home/builder/phux-bin && chown -R builder /home/builder/phux-bin
  cd /home/builder/phux-bin
  su builder -c "makepkg --printsrcinfo" | diff -u .SRCINFO - || { echo ".SRCINFO drifted from makepkg" >&2; exit 1; }
  su builder -c "makepkg --noconfirm" >/dev/null
  pacman -U --noconfirm ./phux-bin-*.pkg.tar.zst >/dev/null
  phux --version
  pacman -Qo /usr/bin/phux
' | grep -q "owned by phux-bin" || fail "the AUR PKGBUILD did not build and install on Arch"
echo "ok: install.sh added the repositories; apt and dnf upgraded $old to $new; phux-bin built on Arch"
