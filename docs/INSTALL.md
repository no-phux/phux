---
audience: humans, contributors
stability: stable
last-reviewed: 2026-10-09
---

# Install

**TL;DR.** Install with Homebrew or the release installer on supported macOS
and Linux hosts. Use numbered releases for stable builds or `next` for the
moving prerelease. Update direct installs with `phux update`, package-managed
installs with their package manager. Desktop alpha and experimental Cockpit
have separate macOS installers.

---

## Platform support

| Platform | Status |
|---|---|
| macOS (Apple Silicon) | Homebrew: yes. Curl/tarball: yes. Source: yes. |
| macOS (x86_64) | Source build only; no official release artifact. Homebrew and the curl installer refuse. |
| Linux x86_64 | Curl/tarball: yes. Homebrew: yes where Linuxbrew supports the host. Source: yes. |
| Linux aarch64 | Curl/tarball: yes. Homebrew: yes where Linuxbrew supports the host. Source: yes. |
| Windows | No. Windows is not supported and is not on the near roadmap. |

## Supported install channels

| Channel | Best for | Status |
|---|---|---|
| Curl installer | Direct install from GitHub release tarballs on supported platforms | Latest GitHub release by default |
| Homebrew | Day-to-day use on Apple-silicon macOS or supported Linuxbrew hosts | Primary binary path where the tap has an artifact |
| Release tarball | Manual install and verification | CI-built tarballs include `phux`, `phux-mcp`, licenses, README, and `.sha256` sidecars |
| From source | Contributors and source-first users | Clone, build, and install with native tools or Nix |
| Agent skills | Harnesses that load SKILL.md | `npx skills add no-phux/skills` |

Public install page: [docs.phux.sh/quickstart/install](https://docs.phux.sh/quickstart/install).

Use the [updater for your install source](#updating). `phux update` refuses
to overwrite Homebrew, Cargo, or Nix installs and prints the native command.

Not supported: `cargo install phux`, Windows, and mise/asdf shims. The
crates.io package is `phux-protocol`, not the CLI.

## Homebrew

Install from the published tap:

```sh
brew trust --tap no-phux/tap # Homebrew 6+
brew tap no-phux/tap
brew install no-phux/tap/phux
```

Homebrew 6 requires the explicit trust decision for a third-party tap; earlier
Homebrew releases do not need that command. This installs both `phux` and
`phux-mcp`. Use a source build if the Formula has not reached your target yet.

On Intel Macs the Formula refuses with "The arm64 architecture is required
for this software"; build from source instead.

## Curl installer

Install the GitHub release assets:

```sh
curl -fsSL https://phux.sh/install | sh
```

The URL serves this repository's `scripts/install.sh` unchanged, as does
`https://phux.sh/install.sh`. You can also fetch it directly from GitHub:

```sh
curl -fsSL https://raw.githubusercontent.com/no-phux/phux/main/scripts/install.sh | sh
```

Read the script before piping it to a shell. It uses POSIX `sh` and runs
under `sh`, `bash`, `dash`, or `ash`.

It verifies the release `.sha256` sidecar before unpacking and transactionally
installs `phux` and `phux-mcp` into `${PHUX_INSTALL_DIR:-$HOME/.local/bin}`.
The previous pair is restored if publication is interrupted or either binary
cannot be published. Set `PHUX_INSTALL_DIR` to choose a different bin
directory. With no `--version`, it uses the latest GitHub release. Pass
`--channel next` (or set `PHUX_CHANNEL=next`) to install the moving
prerelease of green `main` instead:

```sh
curl -fsSL https://phux.sh/install | sh -s -- --channel next
```

The installer prints the next command and, if needed, a `PATH` remedy.
Every portable tarball and installer includes `phux-mcp`; no separate MCP
package is needed. Homebrew stays on stable.

To pin a specific release, pass any tag from the
[releases page](https://github.com/no-phux/phux/releases):

```sh
curl -fsSL https://phux.sh/install | sh -s -- --version vX.Y.Z
```

## GPUIX desktop alpha (native macOS)

The GPUIX desktop is a separate app and release train from Cockpit. With the
phux CLI installed, install the Apple-silicon macOS 27-or-later alpha:

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

This selects the highest published `desktop-vX.Y.Z-alpha.N` version in the
recent release index and verifies the
checksum and ad-hoc app signature before replacing `Phux.app`. It does not
upgrade the CLI or stop a server. Releases are not Apple-notarized; the
installer clears quarantine after verification. Rerun to update, or pin a
published alpha with `sh -s -- --version X.Y.Z-alpha.N`.

The app is a view of server-owned sessions: quitting or crashing the desktop
does not intentionally end the shells. This is not a guarantee against server
failure or reboot. See the [desktop guide](./consumers/desktop.md) for controls
and the current alpha limitations. The installer supports alternate locations
with `--applications-dir` and `--bin-dir`. After install, open `Phux.app` or run
`phux-desktop`.

## Cockpit (native macOS)

Cockpit releases independently under `cockpit-vX.Y.Z` tags. Use its installer:

```sh
curl -fsSL https://phux.sh/install-cockpit | sh
```

The URL serves this repository's `scripts/install-cockpit.sh` unchanged;
read it before running it. By default it installs the latest `cockpit-vX.Y.Z`.
Pin a release with `sh -s -- --version cockpit-vX.Y.Z`, or use
`--channel next` (also `PHUX_CHANNEL=next`) for the build of green `main`:

```sh
curl -fsSL https://phux.sh/install-cockpit | sh -s -- --channel next
```

It verifies the release checksum before unpacking, places `Phux Cockpit.app` in
`/Applications` (`~/Applications` when `/Applications` is not writable),
writes a `phux-cockpit` launcher into
`${PHUX_COCKPIT_BIN_DIR:-${PHUX_INSTALL_DIR:-$HOME/.local/bin}}`,
clears the quarantine attribute, and restores the previous install if
placement fails. After install:

```sh
phux cockpit            # open the app (also `phux-cockpit`)
```

The Homebrew cask installs the same app from the same release assets:

```sh
brew trust --tap no-phux/tap # Homebrew 6+
brew tap no-phux/tap
brew install --cask no-phux/tap/phux-cockpit
```

Cockpit requires Apple silicon macOS 11 or later. Intel Macs have no release
artifact; the curl installer and the cask both refuse there.

For installer-placed apps, **Check for Updates…** (View menu or Settings → About)
checks the installed channel: `CFBundleShortVersionString` against the latest
`cockpit-vX.Y.Z` release for stable, or build SHA against `next` for prereleases.
Install runs `scripts/install-cockpit.sh` with SHA256SUMS verification, atomic
replacement, quarantine clearing, and rollback on placement failure.
After replacement the app relaunches; phux-backed remote sessions remain on
the server. Homebrew, Nix, and development copies refuse self-update and
print the native command.

## Build Desktop from source

Contributors can build the same `Phux.app` locally instead of installing a
published alpha. Install the CLI first, then follow
[contributor setup](./SETUP.md) for the desktop build tools.
From the repository root:

```sh
just doctor desktop
just desktop-install-app
open /Applications/Phux.app
```

Quit an existing `Phux.app` before installing: the command replaces
`/Applications/Phux.app`. It does not replace your installed CLI. On launch,
the app uses that CLI to start or reuse your server and attaches its `default`
session; `PHUX_PROFILE`, `PHUX_SOCKET`, and `PHUX_SESSION` can override the target.

This is an ad-hoc-signed development build, without notarization or automatic
updates. Re-run the install command to rebuild it after updating your checkout.
See the [desktop guide](./consumers/desktop.md) for controls and current limits.

## Agent skills

For harnesses that load Agent Skills:

```sh
npx skills add no-phux/skills
```

This installs `using-phux` and `using-phux-mcp`; select one with
`--skill using-phux` or `--skill using-phux-mcp`. The same skills are served
at `https://phux.sh` (`npx skills add https://phux.sh`).
`phux --skill` and `phux mcp --skill` print version-matched copies compiled
into the binaries. See [Agents](./consumers/agents.md).

## Release tarball

Release tags include target-specific tarballs and checksum sidecars. Pick a
tag from the [releases page](https://github.com/no-phux/phux/releases):

```sh
tag=vX.Y.Z    # a tag from https://github.com/no-phux/phux/releases
target=aarch64-apple-darwin
base="https://github.com/no-phux/phux/releases/download/${tag}"
curl -LO "${base}/phux-${tag}-${target}.tar.gz"
curl -LO "${base}/phux-${tag}-${target}.tar.gz.sha256"
shasum -a 256 -c "phux-${tag}-${target}.tar.gz.sha256"
tar -xzf "phux-${tag}-${target}.tar.gz"
```

Put the extracted `phux` and `phux-mcp` binaries somewhere on `PATH`. Avoid
the very first seeded Linux tarball outside Nix environments; it was built
with a Nix-store dynamic loader and is not portable. Every later release is
a portable CI build.

## From source

Set up the native toolchain using [Contributor setup](./SETUP.md), or use the
Nix dev shell to provision it. Both install binaries into Cargo's bin directory.
With native prerequisites installed:

```sh
git clone https://github.com/no-phux/phux
cd phux
bash scripts/doctor.sh native
cargo install --locked --path crates/phux
cargo install --locked --path crates/phux-mcp
phux
```

With Nix, the equivalent install commands are:

```sh
nix develop -c cargo install --locked --path crates/phux
nix develop -c cargo install --locked --path crates/phux-mcp
```

`phux` with no arguments auto-spawns a server and attaches to it. To detach,
press `Ctrl-A`, release both keys, then press `d`; run `phux` to reattach.
Interactive `phux`, `phux attach`, and `phux new` require both stdin and stdout
to be terminals. Redirected invocations refuse before starting a server or
emitting terminal control bytes; use the headless verbs for scripts and CI.

For development setup and scoped checks, see [Contributor setup](./SETUP.md).
To install a checkout's debug build:

```sh
just install-dev             # build phux + phux-mcp into ~/.cargo/bin
hash -r                      # refresh an older shell's command cache if needed
command -v phux              # inside the checkout: ~/.cargo/bin/phux
```

`install-dev` writes debug binaries to `${CARGO_HOME:-~/.cargo}/bin` only.
`curl | sh` owns `~/.local/bin`. A debug build already uses the `dev`
profile (separate socket and state), so the two cannot steal each other's
sessions. Inside this checkout, direnv / `nix develop` put Cargo's bin
ahead of `~/.local/bin`; leave the repo and `phux` is the user install
again. `just rebuild` installs the next debug build and hot-swaps the
dev-profile server.

### Which documentation applies to my install?

Numbered releases are the stable channel. `next` is a moving prerelease built
from green `main`; a source checkout can contain changes not yet available in
either published channel. A page labeled **checkout behavior** describes that
source, not a promised minimum released version.

Check `phux --version` for the binary and `phux status --json` for the running
server's negotiated capabilities. A newer client does not add capabilities to
an older server. For example, AgentSession procedures require `RESOURCE_KINDS`
in the server's `features`; ordinary pane tools do not require an AgentSession.
Use the [update instructions](#updating) for your install source, and keep
remote peers on the same release. The full compatibility rule is in
[workspace continuity](./operations.md#workspace-continuity-and-update-survival).

## Updating

Choose your install source before running an update:

- **Homebrew:** [upgrade with Homebrew, then hand off the server](#homebrew-1).
- **Curl or release tarball:** use the commands below.
- **Nix:** use the [Nix update procedure](#nixos-and-nix-profiles).
- **Source/Cargo directory:** rebuild from your chosen revision using
  [the source-install commands](#from-source); do not use `cargo install phux`.

For a direct-release install:

```sh
phux update --check     # what is installed, what is published, how it got there
phux update             # install it, then hand a running server off to it
phux channel            # which rail this install follows
phux channel next       # follow green main
phux channel latest     # back to the latest vX.Y.Z
```

Cockpit's **Check for Updates…** only replaces installer-placed apps; see
[Cockpit (native macOS)](#cockpit-native-macos).

Keep peers on the same release: mismatched peers refuse each other at HELLO
([ADR-0071](adr/0071-what-phux-1-0-commits-to.md)). The default channel is the
latest numbered GitHub release. `phux channel next` tracks green `main`
([ADR-0113](adr/0113-next-release-channel.md)); `phux channel latest`
(also `stable`) selects numbered releases. `<bindir>/.phux-channel` stores
the choice for later updates. `phux update --channel next` also switches
channels. Homebrew stays on stable.

On macOS, an installed Phux Cockpit moves with the CLI
([ADR-0138](adr/0138-cockpit-rides-the-next-channel.md)). `phux update`
reinstalls it through the Cockpit installer when it is behind, and
`phux channel next` or `phux channel latest` switches both. `--check` reports
Cockpit on its own `cockpit:` line, and the JSON adds a `cockpit` object.
Homebrew and development copies of Cockpit are reported, never overwritten.
If another `phux` comes first on `PATH`, such as a stale `cargo install` or a
version-manager shim, `phux update` warns and names it. The JSON reports it as
`path_shadowed_by`.

### What `phux update` does

1. Resolves the current GitHub release (the latest `vX.Y.Z`, `--channel next`,
   or the tag you pass to `--version`).
2. Downloads `phux-<tag>-<target>.tar.gz` and its `.sha256` sidecar.
3. Verifies the checksum before unpacking. A mismatch refuses, names both
   digests, and installs nothing.
4. Unpacks to a staging directory on the same filesystem and publishes under
   an update lock with a fsynced recovery journal, so `phux` and a sibling
   `phux-mcp` recover together as the old or new release after an
   interruption. File modes are preserved.
5. Asks a running server to graceful-upgrade (the `phux upgrade` path), so live
   panes survive the swap. Pass `--no-restart` to skip that.

The full trust boundary — including what the checksum does and does not prove
— is [ADR-0074](adr/0074-self-update-trust-boundary.md).

### Install sources it recognizes

`phux update` identifies the install source from the running binary's
symlink-resolved path. It writes only to direct-release installs.

| Source | Recognized by | What `phux update` does |
|---|---|---|
| Direct release | The binary sits in `$PHUX_INSTALL_DIR`, `~/.local/bin`, `~/bin`, `/usr/local/bin`, or `/opt/phux/bin` | Downloads, verifies, replaces atomically |
| Homebrew | The resolved path is inside a `Cellar` (`/opt/homebrew`, `/usr/local`, Linuxbrew, or a relocated `HOMEBREW_PREFIX`) | Refuses; prints `brew upgrade no-phux/tap/phux` |
| Cargo | The binary is in `$CARGO_HOME/bin` (default `~/.cargo/bin`) | Refuses; prints the source-install commands |
| Nix / NixOS | The path is under the Nix store (`/nix/store`, or `$NIX_STORE`) | Refuses; prints `nix profile upgrade phux`, or a flake update plus `nixos-rebuild switch` on NixOS |
| Unknown | Anything else | Refuses, names the path, and lists the locations it does maintain |

For a direct install in another directory, set `PHUX_INSTALL_DIR` to let
`phux update` maintain it. Unknown locations are otherwise refused.

### Homebrew

```sh
brew upgrade no-phux/tap/phux
phux upgrade                    # hand the running server off to the new binary
```

`brew upgrade` replaces the binary; `phux upgrade` hands the running server
over to it. A Homebrew-started server re-execs its own path, preserving panes.

### Direct archives

Use `phux update`, or re-run the curl installer with `--channel next` if that
is your channel.

### NixOS and Nix profiles

Nix store paths are read-only, so `phux update` prints the right command
instead:

```sh
# NixOS, phux from a flake input
nix flake update phux
sudo nixos-rebuild switch

# nix profile install
nix profile upgrade phux

# home-manager: update the input, then
home-manager switch
```

Run `phux upgrade` if the store path is unchanged. If it changed, stop and
restart the server: live upgrade re-execs the same path.

### Checking and previewing

```sh
phux update --check              # report only; never downloads an archive
phux update --check --json       # the stable document (schema_version 1)
phux update --dry-run            # download and verify, install nothing
phux channel next                # follow the moving next prerelease
phux channel latest              # follow the latest numbered release
phux update --version vX.Y.Z     # install a specific stable release (downgrades too)
phux update --no-restart         # replace binaries, leave the server alone
```

`--check` exits 0 whether or not an update exists; read `update_available` in
the JSON document rather than the exit status. A refusal (package-managed,
immutable store, unknown location) exits 2 with the remedy; a failure to fetch,
verify, or install exits 1. Under `--json`, stdout carries only the document
and a failure puts one JSON object on stderr.

### Rolling back

The previous binaries and their version manifest remain in
`.phux-update-backup/` beside the new ones:

```sh
phux update --rollback
```

They are ordinary files, so a release too old to have `phux update` can be
restored by hand:

```sh
cd ~/.local/bin
mv -f .phux-update-backup/phux ./phux
mv -f .phux-update-backup/phux-mcp ./phux-mcp   # if it is installed
rm -rf .phux-update-backup
```

`phux update` keeps exactly one generation of backup — the release you were on
before the last successful update.

## crates.io

crates.io is for the wire library, not for installing the `phux` binary:

```sh
cargo add phux-protocol
```

`cargo install phux` is unsupported. The binary crate and internal
workspace crates are `publish = false`; install the CLI through Homebrew,
the curl installer, release tarballs, or a source build.

## After install

[Quickstart](./QUICKSTART.md) walks through first launch and detach/reattach.
[Coding-agent getting started](./consumers/getting-started.md) selects an integration.
If the binary is missing or the wrong version runs, start with
[installation recovery](./troubleshooting.md#phux-is-not-found-or-the-wrong-version-runs).

## Shell completions

`phux completion SHELL` writes a script for `bash`, `elvish`, `fish`,
`powershell`, or `zsh`, generated from the binary's argument parser. It
contacts no server and reads no config, so it is safe in a shell startup file.

```sh
# zsh — any directory on $fpath works
phux completion zsh > "${fpath[1]}/_phux"

# bash
phux completion bash > ~/.local/share/bash-completion/completions/phux

# fish
phux completion fish > ~/.config/fish/completions/phux.fish
```

Regenerate after upgrading to reflect renamed or removed commands.

