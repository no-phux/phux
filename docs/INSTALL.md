---
audience: humans, contributors
stability: stable
last-reviewed: 2026-10-17
---

# Install

**TL;DR.** One command installs phux on Apple-silicon macOS and on Linux:
`curl -fsSL https://phux.sh/install | sh`. `phux update` keeps it current.
The desktop app has its own one-liner and installs the CLI with it.

---

```sh
curl -fsSL https://phux.sh/install | sh
phux
```

That is the whole install. The installer puts `phux` and `phux-mcp` in
`~/.local/bin` and adds that directory to your shell's `PATH` when it is
missing. It verifies the download against the published SHA-256 before
unpacking, and an interrupted install leaves the previous binaries in place.

`phux` starts a server and attaches to it. Press `Ctrl-A`, then `d`, to
detach; run `phux` again to reattach. Continue with the
[Quickstart](./QUICKSTART.md).

<a id="desktop-app-macos"></a>

## Desktop app

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

Apple silicon, macOS 27 or later. This installs `Phux.app` and installs the
CLI first if it is missing, but never replaces an existing CLI or stops a
running server. The app then updates itself: **Settings > Updates** checks in
the background and installs a verified update with one click. Releases are ad-hoc signed, not
Apple-notarized: the installer checks the checksum and signature, then clears
quarantine. See the [desktop guide](./consumers/desktop.md).

### Linux desktop

The same command installs the Linux desktop on x86-64 and ARM64 systems with
glibc, X11 or Wayland, and a current Vulkan driver. Ubuntu 24.04 is the release build
baseline. The package does not bundle a coordinator: an existing CLI is reused
and a missing CLI is installed separately. Linux keyboard shortcuts include
`Ctrl-Shift-P` for the palette, `Ctrl-Shift-T` for a terminal, and
`Ctrl-Shift-C` / `Ctrl-Shift-V` for copy/paste; plain `Ctrl-C` stays a shell
interrupt. See [desktop prerequisites](./SETUP.md#gpui-desktop) for libraries.
Update Mesa or your vendor's graphics driver if the window stays black:
Mesa 22.3.6's software Vulkan presentation failed qualification even when its
offscreen rendering worked; current Ubuntu 24.04 Mesa passed the visible-window
checks.

The bundle lives at `$XDG_DATA_HOME/phux-desktop` (default
`~/.local/share/phux-desktop`), with a launcher in `~/.local/bin` and a desktop
menu entry. **Settings > Updates** checks Linux releases but does not replace
the running app; rerun the installer to update. To require release-key
verification, use `curl -fsSL https://phux.sh/install-desktop |
PHUX_REQUIRE_SIGNATURE=1 sh`. The coordinator is not replaced or restarted.

The desktop has its own canary on the same moving `next` prerelease:

```sh
curl -fsSL https://phux.sh/install-desktop | sh -s -- --channel next
```

It installs a verified green-`main` build and records the channel in the app;
**Settings > Updates** switches back to Stable at any time. The desktop canary
and the CLI canary are independent, so the app can follow `next` while the
coordinator stays on numbered releases.

## Homebrew

```sh
brew trust --tap no-phux/tap   # Homebrew 6 and later
brew install no-phux/tap/phux
```

Homebrew follows numbered releases only. On Intel Macs the formula refuses;
build [from source](#from-source) instead.

## Updating

```sh
phux update
```

This installs the newest release and hands a running server to it, so live
panes survive. For other install sources, `phux update` refuses and prints
the right command:

| Installed with | Update with |
|---|---|
| Installer or release tarball | `phux update` |
| Homebrew | `brew upgrade no-phux/tap/phux`, then `phux upgrade` |
| Nix profile | `nix profile upgrade phux`, then restart the server |
| NixOS flake input | `nix flake update phux && sudo nixos-rebuild switch`, then restart the server |
| Source checkout | Rebuild with the [source commands](#from-source) |

Keep every host you connect on the same release: mismatched peers refuse
each other at HELLO ([ADR-0071](adr/0071-what-phux-1-0-commits-to.md)).
`phux update --rollback` restores the release you had before the last
update. More options are under [`phux update` in detail](#phux-update-in-detail).

## Platforms

| Platform | Installer | Homebrew | Source |
|---|---|---|---|
| macOS, Apple silicon | yes | yes | yes |
| macOS, Intel | no | no | yes |
| Linux x86_64 | yes | where Linuxbrew runs | yes |
| Linux aarch64 | yes | where Linuxbrew runs | yes |

Windows is not supported and is not on the near roadmap.

## Installer options

Pass options after `sh -s --`:

```sh
curl -fsSL https://phux.sh/install | sh -s -- --version vX.Y.Z   # pin a release
curl -fsSL https://phux.sh/install | sh -s -- --channel next     # follow green main
```

| Option | Effect |
|---|---|
| `--version X.Y.Z` | Install a release from the [releases page](https://github.com/no-phux/phux/releases) instead of the latest. |
| `--channel next` (or `PHUX_CHANNEL=next`) | Install the moving `next` prerelease built from green `main`. |
| `--revision SHA` | With `--channel next`, install the build of that commit. |
| `--install-dir DIR` (or `PHUX_INSTALL_DIR`) | Install somewhere other than `~/.local/bin`. |
| `--no-modify-path` (or `PHUX_NO_MODIFY_PATH=1`) | Leave shell profiles alone; print the `PATH` line instead. |
| `--desktop` | Also install Phux Desktop; `--applications-dir DIR` picks where. |
| `--dry-run` | Print what would be downloaded and where it would go. |

The script is POSIX `sh`; read it at
[phux.sh/install](https://phux.sh/install) before running it. It is this
repository's `scripts/install.sh`, served unchanged, and
`phux.sh/install-desktop` runs it with `--desktop`.

For coding agents that load Agent Skills, add the phux skills with
`npx skills add no-phux/skills`. See [Agents](./consumers/agents.md).

## Other ways to install

### From source

With the native toolchain from [Contributor setup](./SETUP.md):

```sh
git clone https://github.com/no-phux/phux
cd phux
bash scripts/doctor.sh native
cargo install --locked --path crates/phux
cargo install --locked --path crates/phux-mcp
```

With Nix providing the toolchain:

```sh
nix develop -c cargo install --locked --path crates/phux
nix develop -c cargo install --locked --path crates/phux-mcp
```

Both put the binaries in Cargo's bin directory. Contributors installing a
debug build should use `just install-dev`; see [Contributor setup](./SETUP.md).

`cargo install phux` is unsupported: the CLI is not on crates.io. The
published crate is the wire library, `phux-protocol`.

### Release tarball

Every release carries `phux-<tag>-<target>.tar.gz` with a `.sha256` sidecar
and its minisign signature, `.sha256.minisig`, for `aarch64-apple-darwin`,
`x86_64-unknown-linux-gnu`, and `aarch64-unknown-linux-gnu`:

```sh
tag=vX.Y.Z
target=aarch64-apple-darwin
base="https://github.com/no-phux/phux/releases/download/${tag}"
curl -LO "${base}/phux-${tag}-${target}.tar.gz"
curl -LO "${base}/phux-${tag}-${target}.tar.gz.sha256"
curl -LO "${base}/phux-${tag}-${target}.tar.gz.sha256.minisig"
# Trust any key in RELEASE_KEYS in scripts/install.sh; one of them verifies.
keys="$(curl -fsSL https://raw.githubusercontent.com/no-phux/phux/main/scripts/install.sh \
  | sed -n "s/^RELEASE_KEYS='\(.*\)'$/\1/p")"
for key in $keys; do
  minisign -V -H -P "$key" -m "phux-${tag}-${target}.tar.gz.sha256" && break
done
shasum -a 256 -c "phux-${tag}-${target}.tar.gz.sha256"
tar -xzf "phux-${tag}-${target}.tar.gz"
```

Put the extracted `phux` and `phux-mcp` on `PATH`. In `~/.local/bin`,
`phux update` maintains them like an installer install.

### Archived Zig Cockpit

The Zig Cockpit and its installer are removed from `main`. Use the
[desktop app](#desktop-app) for the maintained native client.
The [archive guide](./consumers/cockpit.md) preserves source and recovery
instructions; existing releases and installed copies are not deleted.

## `phux update` in detail

```sh
phux update --check            # what is installed, what is published
phux update --check --json     # the same as a stable document (schema_version 2)
phux update --dry-run          # download and verify, install nothing
phux update --version vX.Y.Z   # a specific release, downgrades included
phux update --no-restart       # replace binaries, leave the server alone
phux update --rollback         # back to the release before the last update
phux channel next              # follow green main from now on
phux channel latest            # back to numbered releases
```

`phux update` reads the channel's release manifest
(`releases/download/channels/stable.json` or `next.json`; one small file, no
GitHub API calls), downloads the release tarball, verifies the minisign
signature on its checksum against the release keys compiled into `phux`,
refuses on a missing or foreign signature or a mismatch with the signed or
manifest SHA-256, and replaces `phux` and `phux-mcp` together
under a lock with a recovery journal. It then asks a running server to
graceful-upgrade, the `phux upgrade` path. The previous pair stays in
`.phux-update-backup/` beside the binaries for `--rollback`. The trust
boundary is [ADR-0074](adr/0074-self-update-trust-boundary.md), with the
signature from [ADR-0178](adr/0178-release-checksums-are-minisign-signed.md).
A stable manifest naming a release older than the installed one is refused
(`update_downgrade_refused`); a downgrade takes `--version`. Phux Desktop
verifies the same signature in-process before it stages an update. The
installer checks the signature when `minisign` is on `PATH`;
`PHUX_REQUIRE_SIGNATURE=1` makes it refuse without one (`0`, the default,
falls back to the SHA-256 alone).

It writes only to direct installs: the binary's resolved path must be in
`$PHUX_INSTALL_DIR`, `~/.local/bin`, `~/bin`, `/usr/local/bin`, or
`/opt/phux/bin`. Homebrew Cellar, Cargo, and Nix store paths get the native
command from the table under [Updating](#updating), and any other location
is refused by name. If another `phux` comes first on `PATH`, `phux update`
warns and names it (`path_shadowed_by` in the JSON).

`--check` exits 0 whether or not an update exists; read `update_available`.
A refusal exits 2 with the remedy, and a fetch, verify, or install failure
exits 1.

The channel choice is stored in `<bindir>/.phux-channel`
([ADR-0113](adr/0113-next-release-channel.md)). It no longer updates or
switches archived Cockpit installations.

On Nix, a live `phux upgrade` re-execs the same store path, so it only helps
when the path is unchanged; a new path needs a server restart.

### Which documentation applies to my install?

Numbered releases are the stable channel. `next` is built from green `main`,
and a source checkout can be newer than both. A page labeled **checkout
behavior** describes the source, not a released version. `phux --version`
names the binary, and `phux status --json` names the running server's
capabilities. The compatibility rule is in
[workspace continuity](./operations.md#workspace-continuity-and-update-survival).

## Shell completions

`phux completion SHELL` prints a completion script for `bash`, `elvish`,
`fish`, `powershell`, or `zsh`. It contacts no server, so it is safe in a
shell startup file:

```sh
phux completion zsh > "${fpath[1]}/_phux"
phux completion bash > ~/.local/share/bash-completion/completions/phux
phux completion fish > ~/.config/fish/completions/phux.fish
```

## Trouble

If `phux` is missing or the wrong version runs, open a new terminal first,
then see
[installation recovery](./troubleshooting.md#phux-is-not-found-or-the-wrong-version-runs).
