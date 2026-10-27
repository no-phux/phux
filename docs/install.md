# Install

Install the CLI/terminal UI below, or choose the [desktop app](#desktop-app).
The CLI install includes `phux` and `phux-mcp`. Run `phux` to start.

```sh
curl -fsSL https://phux.sh/install | sh
```

## Requirements

Published CLI/server builds:

| OS    | Architecture          | Notes                                   |
| ----- | --------------------- | --------------------------------------- |
| macOS | Apple silicon (arm64) | standalone CLI: macOS 11.0 or later     |
| Linux | x86_64                | glibc; there is no musl build           |
| Linux | arm64                 | glibc; there is no musl build           |

Windows: the CLI and server run in WSL 2, with PowerShell launchers. A native
Windows desktop build is not published yet. There is no native Windows
coordinator or Intel Mac build. See [Windows (WSL)](#windows-wsl).

The install script needs `curl` or `wget`, `tar`, and `sha256sum` or `shasum`.
Xcode is only needed for [source builds](./development.md#gpui-desktop).

## macOS permissions

In the desktop, open **Phux Settings → Permissions** (or search for
**Full Disk Access**). **Set up** opens macOS Full Disk Access and selects
the running app in Finder. Drag it into the list and turn it on. Use the
**Local shells & agents** row for the separate local `phux` executable when
the background server also needs access to protected folders. If your
service launches a different install, grant that executable access instead.
macOS owns the approval; phux cannot grant access on your behalf.

Reopen the desktop after granting access. A running server may also need
a restart; save its work first, because quitting the desktop does not stop
shells or agents. Grants on this Mac do not change permissions on a remote
host.

Full Disk Access is optional and only applies to file access. Phux's
terminal and quick-terminal shortcuts do not need Accessibility, Input
Monitoring or Screen Recording. Tools you run may request those separately.
Background notification permissions have their own **Open Settings** button.
See Apple's [file access controls](https://support.apple.com/guide/security/controlling-app-access-to-files-secddd1d86a6/web)
for the operating system's permission boundaries.

## Windows (WSL)

Windows: the CLI and server run in WSL 2, with PowerShell launchers. A native
Windows desktop build is not published yet.

Use Windows Terminal and WSL 2 with Ubuntu 22.04 or newer (x64 or arm64).
The server, panes, configuration, credentials, and agent integrations live
**inside that Linux distribution**, not in Windows. PowerShell can launch
and control them. The terminal UI runs in WSL.

If you do not have WSL, run this in an administrator PowerShell, restart if
Windows requests it, then open Ubuntu once and create your Linux user:

```powershell
wsl --install -d Ubuntu
```

For an existing WSL installation, run `wsl --update` from PowerShell before
installing phux so its Windows console host is current.

In Ubuntu, ensure the download tools are present:

```sh
sudo apt-get update
sudo apt-get install -y curl ca-certificates
curl -fsSL https://phux.sh/install | sh
phux
```

That is sufficient for using phux in a WSL terminal. To also expose `phux`
and `phux-mcp` as PowerShell commands, download and run the
[PowerShell installer](../scripts/install-windows.ps1):

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/no-phux/phux/main/scripts/install-windows.ps1 -OutFile install-windows.ps1
# Inspect the downloaded script before running it.
.\install-windows.ps1 -Distribution Ubuntu
```

Use `wsl --list --quiet` to find your distribution's exact name, such as
`Ubuntu-24.04`, and pass it with `-Distribution`. It must have an initialized,
non-root default Linux user. The installer uses the existing release
installer and its checksum/signature checks. It writes `phux.ps1` and
`phux-mcp.ps1` to `%LOCALAPPDATA%\phux\bin` and adds that directory to your
user `PATH`; it never changes your execution policy or PowerShell profile.
If your policy blocks scripts, follow your organization's policy; on an
unmanaged machine, `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned`
allows locally generated scripts. An administrator-enforced policy wins.

Options: `-Channel next`, `-Version 0.112.0` (stable only),
`-InstallDir C:\Tools\phux`, `-NoModifyPath`, and `-DryRun`.
Open a new PowerShell window after installing:

```powershell
phux --version
phux                         # Linux terminal UI in Windows Terminal
phux ls
phux spawn --cwd /mnt/c/Users/me/project -- bash
phux update                  # updates the Linux installation
```

Arguments are Linux paths: use `/mnt/c/...`, not `C:\...`. WSL maps the
PowerShell working directory when launching the command. Keep repositories
in the Linux filesystem for best performance. Tools and agents you run in
panes must be installed inside WSL. `phux --help` and `phux-mcp --schema`
work from PowerShell too; launchers preserve arguments, exit codes, and
UTF-8 text pipes. For a Windows MCP client, use the native WSL executable
directly, so stdin/stdout bypass PowerShell's text encoders and buffering:

```json
{
  "command": "wsl.exe",
  "args": ["--distribution", "Ubuntu", "--exec", "sh", "-c", "exec ~/.local/bin/phux-mcp"]
}
```

Use your distribution's name in place of `Ubuntu`. Windows PowerShell 5.1
cannot transparently forward a native client's open stdin through a script;
its launcher refuses that mode instead of hanging. PowerShell 7 supports it,
but the direct WSL entrypoint above is recommended for clients on both.
A client running inside WSL can invoke `phux mcp` directly.

Re-running the installer updates the launchers and Linux programs. To
remove the Windows entrypoints, delete only `phux.ps1` and `phux-mcp.ps1`
from the chosen install directory and remove that directory from your user
`PATH`. To remove the Linux programs and state, follow [Uninstall](#uninstall)
inside WSL. Neither step removes your distribution. The terminal UI runs in
Windows Terminal, and WSLg is not required.

## Install script

```sh
curl -fsSL https://phux.sh/install | sh
```

The script downloads the release for your platform, checks its SHA-256 before unpacking anything, and puts `phux` and `phux-mcp` in `~/.local/bin`. If that directory isn't on your `PATH`, it adds it to your shell profile.

Pass options after `sh -s --`:

```sh
curl -fsSL https://phux.sh/install | sh -s -- --help
curl -fsSL https://phux.sh/install | sh -s -- --install-dir ~/bin --no-modify-path
curl -fsSL https://phux.sh/install | sh -s -- --version 0.113.0
curl -fsSL https://phux.sh/install | sh -s -- --channel next
curl -fsSL https://phux.sh/install | sh -s -- --dry-run
```

| Option                          | Does                                                          |
| ------------------------------- | ------------------------------------------------------------- |
| `--install-dir <dir>`           | install somewhere other than `~/.local/bin`                   |
| `--no-modify-path`              | leave your shell profile alone                                |
| `--version <X.Y.Z>`             | install a specific release instead of the latest             |
| `--channel <stable\|next>`      | follow tagged releases (the default), or every `main` build that passes CI |
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

### macOS drag-and-drop download

Download **macos-arm64.dmg** from the latest
[Desktop release](https://github.com/no-phux/phux/releases?q=desktop-v&expanded=true).
Open it, drag **Phux.app** to **Applications**, eject the image, then open
Phux from Applications—not from the mounted image.

The app and DMG are Developer ID signed, Apple-notarized, and stapled.
The bundled CLI/server needs no separate download and never replaces an
existing CLI. See [first launch and recovery](../clients/desktop/README.md#first-launch-and-recovery).

### Shell installer (macOS and Linux)

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

This is the same script as `install --desktop`.

The Linux desktop archive does not include the CLI. On Linux, this script installs the CLI when none is found.

| OS    | Needs                                       | Installs to                                         |
| ----- | ------------------------------------------- | --------------------------------------------------- |
| macOS | Apple silicon, macOS 26.0 or later          | `/Applications`, or `~/Applications` if not writable |
| Linux | x86_64 or arm64, glibc, a working Vulkan driver | `~/.local/share/phux-desktop`, with a menu entry |

The desktop app is rougher than the terminal UI; [Clients](./clients.md#desktop) says what it does today.

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

`phux upgrade` does only the restart: it moves the running server onto whatever `phux` is already installed, again without stopping your panes. Run it after updating some other way, such as `brew upgrade`.

If you're upgrading from 0.110 or older, the new CLI can't stop the old server (`phux upgrade` and `phux kill --server` are refused across protocol majors). Stop it with `pkill -f 'phux server'` or by its pid, then start the new one with `phux server --ensure` (or just run `phux`). Stopping the server ends the programs running in its panes; the new server brings the sessions back without them, and `phux restore` relaunches them. A bare `pkill phux` would also quit attached terminals, agents' `phux mcp` bridges, and the desktop app.

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

If you installed the desktop app, also remove `Phux.app` from `/Applications` or `~/Applications` on macOS, or `~/.local/share/phux-desktop` on Linux. With Homebrew, run `brew uninstall phux` instead of the first `rm`. [File locations](./reference/files.md) lists every path phux writes, including where they move when `XDG_*` variables are set.

## Troubleshooting

- **`no phux release for <os>/<arch>; build from source`**: your platform has no release build. See [Requirements](#requirements).
- **`checksum mismatch for <file>`**: the download was corrupted or altered, and nothing was installed. Run the script again.
- **`phux: command not found` after installing**: open a new shell so it reads the updated profile, or add `~/.local/bin` to your `PATH`. A DMG first launch installs the CLI there and leaves your shell profile unchanged until you choose **Add to PATH** in Phux (or **Phux Settings → Permissions**). The shell installer adds the directory itself when profile changes are allowed.
- Anything else: run `phux doctor`, then see [Troubleshooting](./troubleshooting.md).
