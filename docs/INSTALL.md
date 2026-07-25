---
audience: humans, contributors
stability: stable
last-reviewed: 2026-07-12
---

# Install

**TL;DR.** Homebrew is the recommended install on supported macOS and Linux
machines. The verified curl installer and release tarballs install the same
`phux` and `phux-mcp` binaries. Source builds use the Nix-pinned Rust and Zig
toolchain. Windows and `cargo install phux` are not supported.

---

## Supported install channels

| Channel | Best for | Status |
|---|---|---|
| Homebrew | Day-to-day binary install on supported Homebrew platforms | Primary binary path where the tap has an artifact |
| Curl installer | Scripted install from GitHub release tarballs | Installs the latest GitHub release by default |
| Release tarball | Manual install and verification | CI-built tarballs include `phux`, `phux-mcp`, licenses, README, and `.sha256` sidecars |
| From source | Contributors and source-first users | Clone, build, and install through the Nix-pinned toolchain |

Not supported: `cargo install phux`, Windows, and mise/asdf shims. The
crates.io package is `phux-protocol`, not the CLI.

## Homebrew

Install from the published tap:

```sh
brew install phall1/phux/phux
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

To pin a specific release:

```sh
curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh | bash -s -- --version v0.0.3
```

## Release tarball

Release tags include target-specific tarballs and checksum sidecars:

```sh
tag=v0.0.3
target=aarch64-apple-darwin
base="https://github.com/phall1/phux/releases/download/${tag}"
curl -LO "${base}/phux-${tag}-${target}.tar.gz"
curl -LO "${base}/phux-${tag}-${target}.tar.gz.sha256"
shasum -a 256 -c "phux-${tag}-${target}.tar.gz.sha256"
tar -xzf "phux-${tag}-${target}.tar.gz"
```

Put the extracted `phux` and `phux-mcp` binaries somewhere on `PATH`. Avoid the
seeded `v0.0.1` Linux tarball outside Nix environments; it was built with a
Nix-store dynamic loader and is not portable.

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

## crates.io

crates.io is for the wire library, not for installing the `phux` binary:

```sh
cargo add phux-protocol
```

`cargo install phux is unsupported`. The binary crate and internal
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
