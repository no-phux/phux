---
audience: humans, contributors
stability: stable
last-reviewed: 2026-09-13
---

# Install

**TL;DR.** The curl installer is the universal one-liner. Homebrew is the
recommended day-to-day path on supported macOS and Linux. Source builds
use native tools or Nix. `phux update` maintains a direct-release install;
`--channel next` tracks green `main`. Windows and `cargo install phux` are
not supported.

---

## Supported install channels

| Channel | Best for | Status |
|---|---|---|
| Curl installer | Universal one-liner from GitHub release tarballs | Latest GitHub release by default |
| Homebrew | Recommended day-to-day on supported Homebrew platforms | Primary binary path where the tap has an artifact |
| Release tarball | Manual install and verification | CI-built tarballs include `phux`, `phux-mcp`, licenses, README, and `.sha256` sidecars |
| From source | Contributors and source-first users | Clone, build, and install with native tools or Nix |

The public install page is
[https://docs.phux.sh/quickstart/install](https://docs.phux.sh/quickstart/install).

Once installed, `phux update` is the one command that moves any of them
forward; see [Updating](#updating).

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

The Formula ships arm64 macOS, x86_64 Linux, and arm64 Linux. On an Intel Mac it
refuses with "The arm64 architecture is required for this software" rather than
installing an arm64 binary that cannot run; build from source there.

## Curl installer

The installer is a convenience wrapper over the same GitHub release assets:

```sh
curl -fsSL https://phux.sh/install | sh
```

That URL serves `scripts/install.sh` from this repository byte for byte; the
site build copies it in rather than keeping a second copy, so there is nothing
to drift. `https://phux.sh/install.sh` is the same script under a name your
editor will syntax-highlight, and the raw GitHub URL still works if you would
rather fetch from the repository directly:

```sh
curl -fsSL https://raw.githubusercontent.com/no-phux/phux/main/scripts/install.sh | sh
```

The script is POSIX `sh`, so `sh`, `bash`, `dash`, and `ash` all run it. Read
it before you pipe it anywhere, the way you should with any installer.

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

Homebrew stays on stable. On success, the installer prints the exact command to run next. It prints a copy-paste `PATH` remedy only when that directory is not already on `PATH`.
Every portable tarball and installer path includes `phux-mcp`; there is no
separate MCP package to install.

To pin a specific release, pass any tag from the
[releases page](https://github.com/no-phux/phux/releases):

```sh
curl -fsSL https://phux.sh/install | sh -s -- --version vX.Y.Z
```

## Cockpit (native macOS)

Cockpit is versioned and released independently (`cockpit-vX.Y.Z` tags on a
separate cadence from the CLI above). Install it with its own curl installer:

```sh
curl -fsSL https://phux.sh/install-cockpit | sh
```

That URL serves `scripts/install-cockpit.sh` from this repository byte for
byte, the same way `/install` serves the CLI installer — read it before you
pipe it anywhere. With no `--version`, it installs the latest `cockpit-vX.Y.Z`
release; pin one with `sh -s -- --version cockpit-vX.Y.Z`. It verifies the
release `SHA256SUMS` before unpacking, places **Phux Cockpit.app** in
`/Applications` (`~/Applications` when `/Applications` is not writable),
clears the quarantine attribute, and restores the previous install if
placement fails.

The Homebrew cask installs the same app from the same release assets:

```sh
brew trust --tap no-phux/tap # Homebrew 6+
brew tap no-phux/tap
brew install --cask no-phux/tap/phux-cockpit
```

Cockpit requires Apple silicon macOS 11 or later. Intel Macs have no release
artifact; the curl installer and the cask both refuse there.

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

`phux` with no arguments auto-spawns a server and attaches to it. Detach with
`Ctrl-A d`; run `phux` again to re-attach.
Interactive `phux`, `phux attach`, and `phux new` require both stdin and stdout
to be terminals. Redirected invocations refuse before starting a server or
emitting terminal control bytes; use the headless verbs for scripts and CI.

If you are developing rather than installing, select the relevant native or
Nix setup and scoped checks in [`SETUP.md`](./SETUP.md).
For a checkout you edit continuously, install the current debug build with:

```sh
just install-dev             # build phux + phux-mcp and install both
hash -r                      # refresh an older shell's command cache if needed
command -v phux              # should print ~/.cargo/bin/phux
```

`install-dev` writes the binaries atomically to `${CARGO_HOME:-~/.cargo}/bin`,
matching a normal source install. That directory must precede
`/opt/homebrew/bin` in `PATH`; the standard phux developer environment uses
that order. The Homebrew package can remain installed as a released fallback.
`just rebuild` installs the next build and asks a source-installed server to
re-exec the newly installed binary while preserving its live sessions.

A server already launched from Homebrew cannot change its executable path via
the same-path re-exec mechanism. Detach and stop that server once, verify
`command -v phux` resolves to `~/.cargo/bin/phux`, then start `phux` again.
Existing source-installed servers under `~/.cargo/bin` can upgrade in place;
subsequent `just rebuild` invocations stay entirely on the developer binary.

## Updating

```sh
phux update --check     # what is installed, what is published, how it got there
phux update             # install it, then hand a running server off to it
phux update --channel next   # follow green main instead of the latest vX.Y.Z
```

`phux update` exists because a deployment is a lockstep set: mismatched peers
refuse each other at HELLO. See
[ADR-0071](adr/0071-what-phux-1-0-commits-to.md). Default is the latest
numbered GitHub release. `--channel next` is the opt-in rail that tracks
green `main` ([ADR-0113](adr/0113-next-release-channel.md)); the choice is
remembered in `<bindir>/.phux-channel` so later `phux update` stays on that
rail. Homebrew stays on stable.

### What `phux update` does

1. Resolves the current GitHub release (the latest `vX.Y.Z`, `--channel next`,
   or the tag you pass to `--version`).
2. Downloads `phux-<tag>-<target>.tar.gz` and its `.sha256` sidecar.
3. **Verifies the checksum before unpacking anything.** A mismatch refuses,
   names both digests, and installs nothing.
4. Unpacks to a staging directory beside the installed binaries — same
   filesystem — and publishes under an update lock. Before either binary
   changes, it fsyncs a recovery journal containing the old pair; each atomic
   `rename` is followed by a directory fsync, and publishing that journal as
   the rollback backup is the commit point. A later update or rollback repairs
   any interrupted pre-commit transaction before proceeding, so `phux` and an
   installed sibling `phux-mcp` recover together as the old or new release.
   Replacement preserves the mode of each file being replaced.
5. Asks a running server to graceful-upgrade (the `phux upgrade` path), so live
   panes survive the swap. Pass `--no-restart` to skip that.

The full trust boundary — including what the checksum does and does not prove
— is [ADR-0074](adr/0074-self-update-trust-boundary.md).

### Install sources it recognizes

`phux update` decides how phux was installed from the **symlink-resolved** path
of the running binary, and only ever writes to installs it maintains.

| Source | Recognized by | What `phux update` does |
|---|---|---|
| Direct release | The binary sits in `$PHUX_INSTALL_DIR`, `~/.local/bin`, `~/bin`, `/usr/local/bin`, or `/opt/phux/bin` | Downloads, verifies, replaces atomically |
| Homebrew | The resolved path is inside a `Cellar` (`/opt/homebrew`, `/usr/local`, Linuxbrew, or a relocated `HOMEBREW_PREFIX`) | Refuses; prints `brew upgrade no-phux/tap/phux` |
| Cargo | The binary is in `$CARGO_HOME/bin` (default `~/.cargo/bin`) | Refuses; prints the source-install commands |
| Nix / NixOS | The path is under the Nix store (`/nix/store`, or `$NIX_STORE`) | Refuses; prints `nix profile upgrade phux`, or a flake update plus `nixos-rebuild switch` on NixOS |
| Unknown | Anything else | Refuses, names the path, and lists the locations it does maintain |

An unknown location is a **refusal, not a best-effort overwrite**. If you keep
phux somewhere else on purpose, set `PHUX_INSTALL_DIR` to that directory and
`phux update` will maintain it.

### macOS and Linux

Both platforms use the same command and the same artifact contract. macOS ships
arm64 only; an Intel Mac has no release artifact and `phux update` says so
rather than installing something that cannot exec. Linux ships x86\_64 and
arm64.

### Homebrew

```sh
brew upgrade no-phux/tap/phux
phux upgrade                    # hand the running server off to the new binary
```

`brew upgrade` replaces the binary but does not touch a running server;
`phux upgrade` is the second half. A server that was started from Homebrew
re-execs its own path, so the two steps together preserve live panes.

### Direct archives

If you installed with the curl installer or by unpacking a tarball,
`phux update` is the supported path — it repeats exactly what you did by hand,
with the checksum verified for you. Re-running the curl installer also works
and is equivalent (add `--channel next` if that is the rail you are on):

```sh
curl -fsSL https://phux.sh/install | sh
curl -fsSL https://phux.sh/install | sh -s -- --channel next
```

### NixOS and Nix profiles

Nix store paths are read-only by construction, so `phux update` never modifies
them — detecting the store and printing the right command is the correct
behavior, not a fallback.

```sh
# NixOS, phux from a flake input
nix flake update phux
sudo nixos-rebuild switch

# nix profile install
nix profile upgrade phux

# home-manager: update the input, then
home-manager switch
```

Then `phux upgrade` to move a running server onto the new store path — unless
the store path changed, in which case stop the server and start it again, since
the re-exec mechanism replays the *same* path.

### Checking and previewing

```sh
phux update --check              # report only; never downloads an archive
phux update --check --json       # the stable document (schema_version 1)
phux update --dry-run            # download and verify, install nothing
phux update --channel next       # follow the moving next prerelease
phux update --version vX.Y.Z     # install a specific stable release (downgrades too)
phux update --no-restart         # replace binaries, leave the server alone
```

`--check` exits 0 whether or not an update exists; read `update_available` in
the JSON document rather than the exit status. A refusal (package-managed,
immutable store, unknown location) exits 2 with the remedy; a failure to fetch,
verify, or install exits 1. Under `--json`, stdout carries only the document
and a failure puts one JSON object on stderr.

### Rolling back

The previous binaries are kept in `.phux-update-backup/` beside the new ones,
with a manifest naming the version they are:

```sh
phux update --rollback
```

They are ordinary files in an ordinary directory, which is the point: if the
release you installed is old enough that it has no `phux update` verb, restore
by hand and nothing is lost.

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

[Quickstart](./QUICKSTART.md) is the first run.
[Agents](./consumers/agents.md) is the headless contract.

## Shell completions

`phux completion SHELL` writes a completion script to stdout for `bash`,
`elvish`, `fish`, `powershell`, or `zsh`. The script is generated from the
binary's own argument parser, so it can only ever offer verbs the installed
build actually accepts. It contacts no server and reads no config, which is
what makes it safe to call from a shell startup file.

```sh
# zsh — any directory on $fpath works
phux completion zsh > "${fpath[1]}/_phux"

# bash
phux completion bash > ~/.local/share/bash-completion/completions/phux

# fish
phux completion fish > ~/.config/fish/completions/phux.fish
```

Regenerate after upgrading phux. A stale script keeps completing verbs the
new binary may have renamed or dropped.

## Platform support

| Platform | Status |
|---|---|
| macOS (Apple Silicon) | Homebrew: yes. Curl/tarball: yes. Source: yes. |
| macOS (x86_64) | Not supported. No official release artifact; Homebrew and the curl installer both refuse. Source: yes. |
| Linux x86_64 | Curl/tarball: yes. Homebrew: yes where Linuxbrew supports the host. Source: yes. |
| Linux aarch64 | Curl/tarball: yes. Homebrew: yes where Linuxbrew supports the host. Source: yes. |
| Windows | No. Windows is not supported and is not on the near roadmap. |
