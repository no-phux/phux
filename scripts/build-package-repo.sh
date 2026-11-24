#!/usr/bin/env bash
# Build the signed apt and dnf repository metadata for the newest releases'
# packages (docs/releasing.md#linux-packages) into SITE.
#
#   PHUX_PACKAGES_GPG_KEY=<armored secret signing key> \
#     bash scripts/build-package-repo.sh <site> [<packages-dir>]
#
# The repositories live on GitHub releases: each package stays on its own
# vX.Y.Z release, and the metadata goes on the `repodata` release, so
#   apt: deb <releases/download> repodata/   (a flat repository)
#   dnf: baseurl=<releases/download>          (dnf reads repodata/repomd.xml)
# SITE mirrors that tree: SITE/vX.Y.Z/<packages> and SITE/repodata/<metadata>.
# Everything is rebuilt from the KEEP newest releases each time; nothing
# carries state, and older releases drop out.
#
# A packages dir (SITE-shaped: vX.Y.Z/<packages>) replaces the download, for
# a rehearsal; PHUX_PACKAGES_PUBLIC_KEY (the key the signer must match) and
# PHUX_PACKAGES_URL (where SITE is served) go with it.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

KEEP=3
URL="${PHUX_PACKAGES_URL:-https://github.com/no-phux/phux/releases/download}"
PUBLIC_KEY="${PHUX_PACKAGES_PUBLIC_KEY:-$ROOT/packaging/phux.asc}"

die() {
  phux_error "$*"
  exit 1
}

[[ $# -ge 1 && $# -le 2 ]] || die "usage: build-package-repo.sh <site> [<packages-dir>]"
site="$1"
[[ -n ${PHUX_PACKAGES_GPG_KEY:-} ]] || die "PHUX_PACKAGES_GPG_KEY is not set; the repositories must be signed"
for tool in apt-ftparchive createrepo_c gpg; do
  command -v "$tool" >/dev/null || die "$tool is required (apt install apt-utils createrepo-c gnupg)"
done
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

export GNUPGHOME="$work/gnupg"
mkdir -m 700 "$GNUPGHOME"
printf '%s\n' "$PHUX_PACKAGES_GPG_KEY" | gpg --batch --quiet --import
signer="$(gpg --list-secret-keys --with-colons | awk -F: '$1 == "fpr" { print $10; exit }')"
published="$(gpg --show-keys --with-colons "$PUBLIC_KEY" | awk -F: '$1 == "fpr" { print $10; exit }')"
[[ -n $signer && $signer == "$published" ]] \
  || die "the signing key ($signer) is not $PUBLIC_KEY ($published); clients would refuse the repository"

rm -rf "$site"
mkdir -p "$site"
if [[ $# -eq 2 ]]; then
  cp -R "$2"/. "$site/"
else
  found=0
  while read -r tag && ((found < KEEP)); do
    if gh release download "$tag" --repo no-phux/phux --dir "$site/$tag" \
      --pattern 'phux_*.deb' --pattern 'phux-*.rpm' 2>/dev/null; then
      found=$((found + 1))
      phux_info "including $tag"
    fi
  done < <(gh release list --repo no-phux/phux --exclude-drafts --limit 40 --json tagName \
    --jq '.[].tagName | select(test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))')
  ((found > 0)) || die "no release carries .deb and .rpm packages yet"
fi
cd "$site"

# dnf first: createrepo_c replaces repodata/ wholesale. Fixed names and gzip,
# so every publish overwrites the same assets and old dnf reads them.
createrepo_c --quiet --no-database --simple-md-filenames --general-compress-type=gz .
gpg --batch --yes --local-user "$signer" --armor --detach-sign repodata/repomd.xml

# apt: a flat repository whose Filename fields are relative to URL.
for dir in v*/; do
  apt-ftparchive packages "${dir%/}"
done >repodata/Packages
gzip -9kn repodata/Packages
# Suite names the directory, or apt warns of a conflicting distribution.
apt-ftparchive -o APT::FTPArchive::Release::Origin=phux -o APT::FTPArchive::Release::Label=phux \
  -o APT::FTPArchive::Release::Suite=repodata release repodata >"$work/Release"
mv "$work/Release" repodata/Release
gpg --batch --yes --local-user "$signer" --clearsign -o repodata/InRelease repodata/Release
gpg --batch --yes --local-user "$signer" --armor --detach-sign -o repodata/Release.gpg repodata/Release

cp "$PUBLIC_KEY" repodata/phux.asc
cat >repodata/phux.repo <<EOF
[phux]
name=phux
baseurl=$URL
enabled=1
gpgcheck=1
repo_gpgcheck=1
metadata_expire=1h
gpgkey=$URL/repodata/phux.asc
EOF
phux_ok "built the apt and dnf repositories in $site"
