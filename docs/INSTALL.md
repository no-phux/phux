---
audience: humans, contributors
stability: stable
last-reviewed: 2026-08-07
---

# Install

**TL;DR.** Homebrew is the recommended install on supported macOS and Linux
machines. The verified curl installer and release tarballs install the same
`phux` and `phux-mcp` binaries. Source builds use the Nix-pinned Rust and Zig
toolchain. `phux update` maintains a direct-release install in place and prints
the exact native command for a Homebrew, Cargo, or Nix one. Windows and
`cargo install phux` are not supported.

---

## Supported install channels

| Channel | Best for | Status |
|---|---|---|
| Homebrew | Day-to-day binary install on supported Homebrew platforms | Primary binary path where the tap has an artifact |
| Curl installer | Scripted install from GitHub release tarballs | Installs the latest GitHub release by default |
| Release tarball | Manual install and verification | CI-built tarballs include `phux`, `phux-mcp`, licenses, README, and `.sha256` sidecars |
| From source | Contributors and source-first users | Clone, build, and install through the Nix-pinned toolchain |

Once installed, `phux update` is the one command that moves any of them
forward; see [Updating](#updating).

Not supported: `cargo install phux`, Windows, and mise/asdf shims. The
crates.io package is `phux-protocol`, not the CLI.

## Homebrew

Install from the published tap:

```sh
brew install phall1/tap/phux
```

This installs both `phux` and `phux-mcp`. Use a source build if the Formula has
not reached your target yet.

The Formula ships arm64 macOS, x86_64 Linux, and arm64 Linux. On an Intel Mac it
refuses with "The arm64 architecture is required for this software" rather than
installing an arm64 binary that cannot run; build from source there.

## Curl installer

The installer is a convenience wrapper over the same GitHub release assets:

```sh
curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh | bash
```

It verifies the release `.sha256` sidecar before unpacking and installs
`phux` and `phux-mcp` into `${PHUX_INSTALL_DIR:-$HOME/.local/bin}`. Set
`PHUX_INSTALL_DIR` to choose a different bin directory. With no `--version`, it
uses the latest GitHub release.
Every portable tarball and installer path includes `phux-mcp`; there is no
separate MCP package to install.

To pin a specific release, pass any tag from the
[releases page](https://github.com/phall1/phux/releases):

```sh
curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh | bash -s -- --version vX.Y.Z
```

## Release tarball

Release tags include target-specific tarballs and checksum sidecars. Pick a
tag from the [releases page](https://github.com/phall1/phux/releases):

```sh
tag=vX.Y.Z    # a tag from https://github.com/phall1/phux/releases
target=aarch64-apple-darwin
base="https://github.com/phall1/phux/releases/download/${tag}"
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

Installing from source uses the Nix dev shell to pin the Rust toolchain and the
Zig compiler libghostty's build needs. The commands still install binaries into
Cargo's bin directory:

```sh
git clone https://github.com/phall1/phux
cd phux
nix develop -c cargo install --locked --path crates/phux
nix develop -c cargo install --locked --path crates/phux-mcp
phux
```

`phux` with no arguments auto-spawns a server and attaches to it. Detach with
`Ctrl-A d`; run `phux` again to re-attach.

If you are developing rather than installing, use `nix develop` or `direnv
allow` and then the `just` commands in [`QUICKSTART.md`](./QUICKSTART.md).
For a checkout you edit continuously, install the current debug build with:

```sh
direnv allow                 # once per checkout
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
```

### Why phux ships an update command

A phux deployment is a lockstep set. [ADR-0071](../ADR/0071-what-phux-1-0-commits-to.md)
freezes the consumer surface at 1.0 but deliberately leaves the **wire** on its
own `0.x` line under [ADR-0061](../ADR/0061-capabilities-add-versions-break.md),
where a minor protocol bump is a fleet-wide break with no grace window:
mismatched peers refuse each other at HELLO rather than half-working. The
compatibility unit is therefore the **release**, not the frame — a server, the
clients attached to it, its satellites, and its relays must all run the same
one. That is why a one-command update path is 1.0 scope rather than a
convenience: a fleet that is hard to move between releases is a fleet that will
sit on a mismatch.

### What `phux update` does

1. Resolves the current GitHub release (or the tag you pass to `--version`).
2. Downloads `phux-<tag>-<target>.tar.gz` and its `.sha256` sidecar.
3. **Verifies the checksum before unpacking anything.** A mismatch refuses,
   names both digests, and installs nothing.
4. Unpacks to a staging directory beside the installed binaries — same
   filesystem — and replaces them with an atomic `rename`, preserving the mode
   of the file being replaced. `phux-mcp` is replaced alongside `phux` when it
   is installed next to it, because a new `phux` beside a stale `phux-mcp` is
   the mismatch this command exists to prevent.
5. Asks a running server to graceful-upgrade (the `phux upgrade` path), so live
   panes survive the swap. Pass `--no-restart` to skip that.

The full trust boundary — including what the checksum does and does not prove
— is [ADR-0074](../ADR/0074-self-update-trust-boundary.md).

### Install sources it recognizes

`phux update` decides how phux was installed from the **symlink-resolved** path
of the running binary, and only ever writes to installs it maintains.

| Source | Recognized by | What `phux update` does |
|---|---|---|
| Direct release | The binary sits in `$PHUX_INSTALL_DIR`, `~/.local/bin`, `~/bin`, `/usr/local/bin`, or `/opt/phux/bin` | Downloads, verifies, replaces atomically |
| Homebrew | The resolved path is inside a `Cellar` (`/opt/homebrew`, `/usr/local`, Linuxbrew, or a relocated `HOMEBREW_PREFIX`) | Refuses; prints `brew upgrade phall1/tap/phux` |
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
brew upgrade phall1/tap/phux
phux upgrade                    # hand the running server off to the new binary
```

`brew upgrade` replaces the binary but does not touch a running server;
`phux upgrade` is the second half. A server that was started from Homebrew
re-execs its own path, so the two steps together preserve live panes.

### Direct archives

If you installed with the curl installer or by unpacking a tarball,
`phux update` is the supported path — it repeats exactly what you did by hand,
with the checksum verified for you. Re-running the curl installer also works
and is equivalent:

```sh
curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh | bash
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
phux update --version vX.Y.Z     # install a specific release (downgrades too)
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

## First run: persistent session + agent loop

After install, run:

```sh
phux
```

`phux` with no arguments auto-spawns a server and attaches to a shell-backed
session. Detach with `Ctrl-A d`; the server keeps the shell alive. Run `phux`
again to re-attach.

From a second terminal, drive the same persistent pane through the agent loop:

```sh
phux ls --json
phux send-keys . "printf '%s\n' phux-ready | tr a-z A-Z" Enter
phux wait --until "PHUX-READY" --timeout 10 .
phux snapshot --json --scrollback 50 . > phux-screen.json
```

That is the read -> act -> wait -> read pattern from
[`consumers/agents.md`](./consumers/agents.md): read state, send or run work in
the pane, wait for observable output, then snapshot again. It uses the same
server and PTY as the interactive TUI. phux does not promise live PTY
resurrection; workspace restore starts new processes instead of reviving an old
PTY.

## Drive it from an agent

The agent surface ships with the same release artifact — nothing extra to
install. The MCP adapter is its own bundled binary:

```sh
phux-mcp     # JSON-RPC over stdio; wire it into your MCP client
```

Tool catalog and JSON contracts: [`consumers/mcp.md`](./consumers/mcp.md). The
plain-CLI version of the same surface: [`consumers/agents.md`](./consumers/agents.md).

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
