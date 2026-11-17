# Linux packages

Each stable CLI release ships `.deb` and `.rpm` packages, and signed apt and
dnf repositories serve them from GitHub releases. The packages stay on their
`vX.Y.Z` releases; the metadata is the `repodata` release, so both package
managers read `https://github.com/no-phux/phux/releases/download`: apt as a
flat repository (`repodata/`), dnf through `repodata/repomd.xml`. Customers
add them through the [install script](../install.md#linux-package-repositories).

## How a release reaches apt and dnf

1. `release.yml`'s `cli` job repackages the two Linux tarballs with nfpm
   ([`scripts/pack-linux-packages.sh`](../../scripts/pack-linux-packages.sh)),
   signs the `.rpm` files, and attaches the packages with minisigned
   checksums.
2. Its `packages` job calls [`packages.yml`](../../.github/workflows/packages.yml).
   That rebuilds both repositories from the three newest releases that carry
   packages ([`scripts/build-package-repo.sh`](../../scripts/build-package-repo.sh)),
   signs `InRelease`, `Release.gpg` and `repomd.xml`, and replaces the
   `repodata` release's assets. Nothing carries state between runs; older
   releases drop out.
3. The same workflow writes the AUR `phux-bin` PKGBUILD
   ([`scripts/aur-pkgbuild.sh`](../../scripts/aur-pkgbuild.sh)) and pushes
   it to `aur.archlinux.org` only when `AUR_SSH_PRIVATE_KEY` is set.

To republish without a release, dispatch `packages.yml` alone:
`gh workflow run packages.yml --ref main`.

## Keys and secrets

| What | Where |
| ---- | ----- |
| Public key, fingerprint `0F3D4046E72D0CB23784A58B043A18DFD4D1A4D5` | [`packaging/phux.asc`](../../packaging/phux.asc), embedded in `install.sh`, and `repodata/phux.asc` |
| `PHUX_PACKAGES_GPG_KEY` | the signing subkey only (`0D9308B285DDE8E1`), no passphrase; the primary key's secret part is not in it |
| Full secret key (primary and subkey) | the maintainer's offline backup |
| `AUR_SSH_PRIVATE_KEY` | optional: the AUR account's SSH key; unset skips the AUR push |

`build-package-repo.sh` refuses a signing key whose primary is not
`packaging/phux.asc`. If the subkey leaks, revoke it with the primary from
the backup, add a new signing subkey, update `packaging/phux.asc` and the
secret, and rebuild `install.sh` (`sh scripts/generate-install.sh`).
Existing customers' keyrings hold the old public key: they need the new one
before packages signed by the new subkey install.

## Rehearse

`bash scripts/test-linux-packages.sh` packages two real releases with a
throwaway key, installs the older one with `install.sh` on fresh Ubuntu and
Fedora containers, publishes the newer one, checks `apt-get upgrade` and
`dnf upgrade` pick it up, and builds the PKGBUILD on Arch. It needs docker
and `gh`. Run it after changing any of the scripts above.

## Ceilings

- GitHub Pages was the plan, but the org has Pages creation turned off. An
  org admin can turn it on, or a CDN can serve the same tree at
  `pkg.phux.sh` (a DNS change); then point `REPO_URL` in
  `scripts/installer/repo.sh` and `URL` in `build-package-repo.sh` there.
  Machines that already added the repository keep the old URL.
- Assets are replaced in place, so for a moment after a publish a cached
  index can name data that changed; apt or dnf then fails that one refresh
  and succeeds on the next.
- Packages need glibc 2.35, like the tarballs: Debian 12, Ubuntu 22.04,
  Fedora, RHEL 10. RHEL 9 is refused by the package's glibc dependency.
- Phux Rune is not packaged: it ships on its own train and updates itself
  in place under `~/.local/share`.
