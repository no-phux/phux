# Install

Release builds exist for macOS on Apple silicon, Linux x86_64, and Linux
arm64 (glibc). There is no Windows, Intel Mac, or musl build.

## Installer

```sh
curl -fsSL https://phux.sh/install | sh
```

This puts `phux` and `phux-mcp` in `~/.local/bin` and adds that directory to
your shell's `PATH` if it is not there yet. Options go after `sh -s --`:

```sh
curl -fsSL https://phux.sh/install | sh -s -- --help
curl -fsSL https://phux.sh/install | sh -s -- --install-dir ~/bin --no-modify-path
curl -fsSL https://phux.sh/install | sh -s -- --version X.Y.Z
curl -fsSL https://phux.sh/install | sh -s -- --dry-run
```

The installer always checks the archive's SHA-256. If `minisign` is on your
`PATH` it also checks the release signature; set `PHUX_REQUIRE_SIGNATURE=1`
to refuse an install without that check.

## Homebrew

```sh
brew trust --tap no-phux/tap
brew install no-phux/tap/phux
```

## Desktop app

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

Installs Phux Desktop, and the CLI too if you don't have it. macOS needs
Apple silicon and macOS 27 or later; the app is ad-hoc signed and not
notarized. On Linux (x86_64 or arm64) the app needs a working Vulkan driver.
See [clients](./clients.md) for what the desktop app does today.

## Updating

```sh
phux update            # install the latest release; running shells survive
phux update --check    # just say whether there is one
phux update --rollback # put the previous binaries back
```

`phux update` replaces both binaries, then restarts the server in place so
your panes keep running. `phux upgrade` does only the second half: it
restarts the server onto whatever binary is already on disk.

There are two channels. `stable` is the tagged releases. `next` is rebuilt
from every green commit on `main`.

```sh
phux channel           # show the current channel
phux channel next      # switch and update
```

If phux came from Homebrew, Nix, or `cargo install`, `phux update` prints the
command for that package manager instead of touching the files.

## Shell completion

```sh
phux completion zsh > ~/.zfunc/_phux
phux completion bash > ~/.local/share/bash-completion/completions/phux
phux completion fish > ~/.config/fish/completions/phux.fish
```

`elvish`, `nu`, and `powershell` work too. Regenerate after an update.

## From source

You need the Rust version pinned in [`rust-toolchain.toml`](../rust-toolchain.toml),
a C toolchain, `curl`, and `tar`. The build downloads a checksummed prebuilt
libghostty for the three release targets; anywhere else it builds libghostty
with Zig 0.16.

```sh
git clone https://github.com/no-phux/phux
cd phux
cargo install --locked --path crates/phux
cargo install --locked --path crates/phux-mcp
```

For a full development setup, see [development](./development.md).

## Uninstall

There is no uninstall command. Stop the server, then delete the files:

```sh
phux service uninstall   # only if you ran `phux service install`
phux kill --server
rm ~/.local/bin/phux ~/.local/bin/phux-mcp
rm -rf ~/.config/phux ~/.local/state/phux ~/.local/share/phux
```

[Files](./reference/files.md) lists every path phux writes, including the
ones that move when `XDG_*` variables are set.
