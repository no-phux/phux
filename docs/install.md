# Install

phux is one command-line program, `phux`, plus `phux-mcp` for coding agents. Install it with the script below, then run `phux`.

```sh
curl -fsSL https://phux.sh/install | sh
```

## Requirements

Release builds exist for three platforms:

| OS    | Architecture                | Notes                         |
| ----- | --------------------------- | ----------------------------- |
| macOS | Apple silicon (arm64)       |                               |
| Linux | x86_64                      | glibc; there is no musl build |
| Linux | arm64                       | glibc; there is no musl build |

There is no build for Windows or Intel Macs. On anything else, [build from source](#build-from-source).

The install script needs `curl` or `wget`, `tar`, and `sha256sum` or `shasum`.

## Install script

```sh
curl -fsSL https://phux.sh/install | sh
```

The script downloads the release for your platform, checks its SHA-256 before unpacking anything, and puts `phux` and `phux-mcp` in `~/.local/bin`. If that directory isn't on your `PATH`, it adds it to your shell profile.

Pass options after `sh -s --`:

```sh
curl -fsSL https://phux.sh/install | sh -s -- --help
curl -fsSL https://phux.sh/install | sh -s -- --install-dir ~/bin --no-modify-path
curl -fsSL https://phux.sh/install | sh -s -- --version 1.2.3
curl -fsSL https://phux.sh/install | sh -s -- --channel next
curl -fsSL https://phux.sh/install | sh -s -- --dry-run
```

| Option                          | Does                                                          |
| ------------------------------- | ------------------------------------------------------------- |
| `--install-dir <dir>`           | install somewhere other than `~/.local/bin`                   |
| `--no-modify-path`              | leave your shell profile alone                                |
| `--version <X.Y.Z>`             | install a specific release instead of the latest             |
| `--channel <stable\|next>`      | follow tagged releases (the default) or every green `main`    |
| `--desktop`                     | also install the desktop app                                  |
| `--dry-run`                     | print what would be downloaded and where, and install nothing |

**Signature checks need `minisign`.** With `minisign` on your `PATH`, the script also verifies the release signature. Set `PHUX_REQUIRE_SIGNATURE=1` to refuse to install without that check.

## Homebrew

```sh
brew trust --tap no-phux/tap
brew install no-phux/tap/phux
```

Homebrew 6 and later ask you to trust a third-party tap before installing from it; on older versions, skip the first line. This installs `phux` and `phux-mcp`. Update with `brew upgrade no-phux/tap/phux`.

## Desktop app

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

This installs Phux Desktop, and the `phux` command too if you don't have it; the app uses it to reach the server. It's the same script as `install --desktop`.

| OS    | Needs                                       | Installs to                                         |
| ----- | ------------------------------------------- | --------------------------------------------------- |
| macOS | Apple silicon, macOS 27 or later            | `/Applications`, or `~/Applications` if not writable |
| Linux | x86_64 or arm64, glibc, a working Vulkan driver | `~/.local/share/phux-desktop`, with a menu entry |

**The macOS app is ad-hoc signed and not notarized.** It's rougher than the terminal UI; [Clients](./clients.md#desktop) says what it does today.

## Check it worked

```sh
phux --version
phux doctor
```

`phux doctor` checks your config, the socket, and the server, and says what to fix. Then [start your first session](./getting-started.md).

## Update

```sh
phux update            # install the latest release
phux update --check    # say whether there is one, and change nothing
phux update --rollback # put the previous version back
```

`phux update` downloads the new release, checks it, replaces both programs, and restarts the server in place. **Your panes keep running through an update**, and attached clients reconnect on their own.

`phux upgrade` is the second half on its own: it restarts the running server onto whatever `phux` is already installed. Run it after updating some other way, such as `brew upgrade`.

If you installed with Homebrew, Nix, or `cargo install`, `phux update` changes nothing and prints the command for that package manager instead.

### Channels

```sh
phux channel           # show the channel you follow
phux channel next      # follow every green build of main, and update now
phux channel stable    # go back to tagged releases
```

`stable` is the tagged releases and the default. `next` is rebuilt from every commit on `main` that passes CI.

## Shell completion

```sh
phux completion zsh > ~/.zfunc/_phux
phux completion bash > ~/.local/share/bash-completion/completions/phux
phux completion fish > ~/.config/fish/completions/phux.fish
```

`elvish`, `nu`, and `powershell` work too. For zsh, `~/.zfunc` must be on your `$fpath`. Regenerate the script after an update, or it completes commands the new version doesn't have.

## Build from source

You need the Rust toolchain pinned in the repository, a C toolchain, `curl`, and `tar`.

```sh
git clone https://github.com/no-phux/phux
cd phux
cargo install --locked --path crates/phux
cargo install --locked --path crates/phux-mcp
```

On the three release platforms the build downloads a prebuilt, checksummed copy of the terminal emulator library; anywhere else it builds it with Zig 0.16. [Development](./development.md) covers a full setup.

**A build from a checkout uses its own server.** It runs under a separate `dev` profile with its own socket and state, so it never touches the sessions of an installed `phux`. Install it with `cargo install` to make it the one you use.

## Uninstall

There is no uninstall command. Stop the server, then delete the files:

```sh
phux service uninstall   # only if you ran phux service install
phux kill --server       # ends every session
rm ~/.local/bin/phux ~/.local/bin/phux-mcp
rm -rf ~/.config/phux ~/.local/state/phux ~/.local/share/phux
```

Remove `Phux.app` from `/Applications` or `~/Applications` if you installed the desktop app. With Homebrew, run `brew uninstall phux` instead of the first `rm`. [File locations](./reference/files.md) lists every path phux writes, including where they move when `XDG_*` variables are set.

## Troubleshooting

- **`no phux release for <os>/<arch>; build from source`**: your platform has no release build. See [Requirements](#requirements).
- **`checksum mismatch for <file>`**: the download was corrupted or altered, and nothing was installed. Run the script again.
- **`phux: command not found` after installing**: open a new shell so it reads the updated profile, or add `~/.local/bin` to your `PATH`.
- Anything else: run `phux doctor`, then see [Troubleshooting](./troubleshooting.md).
