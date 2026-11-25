---
group: start
order: 1
slug: installation
---

# Install

How to install, update, and remove the phux CLI, server, and desktop app.

```sh
curl -fsSL https://phux.sh/install | sh
phux doctor
```

This installs `phux` and `phux-mcp`. `phux doctor` says what to fix, then
[start your first session](./getting-started.md).

## Requirements

| OS          | Architecture   | Notes                                          |
| ----------- | -------------- | ---------------------------------------------- |
| macOS 11+   | Apple silicon  | no Intel build                                 |
| Linux       | x86_64, arm64  | glibc 2.35+ (Debian 12, Ubuntu 22.04); no musl |
| Windows     | x64, arm64     | the Linux build in [WSL 2](#windows-wsl)       |

The install script needs `curl` or `wget`, `tar`, and `sha256sum` or
`shasum`.

## Install script

```sh
curl -fsSL https://phux.sh/install | sh -s -- --help
```

`--help` lists every option (channel, version pin, install directory,
`--rune`, `--dry-run`). The script checks the SHA-256 before unpacking, and
the release signature too when `minisign` is on `PATH`;
`PHUX_REQUIRE_SIGNATURE=1` refuses to install without it.

- Debian, Ubuntu, Fedora, RHEL: it adds the
  [apt or dnf repository](#linux-package-repositories) with sudo, so phux
  updates with the system. `--no-repo` installs the archive instead.
- Elsewhere: `phux` and `phux-mcp` go in `~/.local/bin`, added to your
  shell profile unless `--no-modify-path`.
- Linux: it offers `phux service install --adopt` (a systemd user unit plus
  linger) so sessions survive logout and reboot. `--no-service` skips it.
- macOS: a stable release also installs [phux Server](#macos-permissions).

## Linux package repositories

Signed apt and dnf repositories carry the stable channel. By hand:

```sh
R=https://github.com/no-phux/phux/releases/download
# Debian, Ubuntu
sudo curl -fsSLo /etc/apt/keyrings/phux.asc $R/repodata/phux.asc
echo "deb [signed-by=/etc/apt/keyrings/phux.asc] $R repodata/" | sudo tee /etc/apt/sources.list.d/phux.list
sudo apt update && sudo apt install phux
# Fedora, RHEL
sudo curl -fsSLo /etc/yum.repos.d/phux.repo $R/repodata/phux.repo
sudo dnf install phux
```

The signing key is
[`packaging/phux.asc`](https://github.com/no-phux/phux/blob/main/packaging/phux.asc),
fingerprint `0F3D4046E72D0CB23784A58B043A18DFD4D1A4D5`. Each release also
carries `.deb` and `.rpm` files. On Arch, install `phux-bin` from the AUR.

## Homebrew

```sh
brew install no-phux/tap/phux
```

## macOS permissions

macOS charges every program in a pane to the server, so the server holds
the Full Disk Access, Screen Recording, and Accessibility grants for
everything you run in phux. phux itself needs none of them, and every grant
is optional.

The install script and `phux update` keep a signed **phux Server** app at
`~/Library/Application Support/sh.phux/Server/Versions/Current`, so System
Settings lists one row and a grant survives updates. Homebrew, Nix, and
source builds install no bundle. `next` builds are signed ad hoc, so macOS
treats each one as a new app.

```sh
phux server permissions                          # what the server holds, and which app it is charged to
phux server permissions --open full-disk-access  # open that Settings pane and print what to add
```

A server auto-started from Terminal or Ghostty is charged to that terminal.
To have **phux Server** hold the grants itself, run it under
`phux service install`. Restart the server after granting; a restart ends
the programs in panes.

## Windows (WSL)

The server, panes, config, and agents live inside a WSL 2 distribution
(Ubuntu 22.04 or newer). In an administrator PowerShell, if you have no WSL
yet:

```powershell
wsl --install -d Ubuntu
```

Then, in Ubuntu:

```sh
sudo apt-get install -y curl ca-certificates
curl -fsSL https://phux.sh/install | sh
```

The distribution needs an initialized non-root default user. To also run
`phux` and `phux-mcp` from PowerShell:

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/no-phux/phux/main/scripts/install-windows.ps1 -OutFile install-windows.ps1
.\install-windows.ps1 -Distribution Ubuntu   # a name from wsl --list --quiet
```

 It writes launchers to
`%LOCALAPPDATA%\phux\bin` and adds that to your user `PATH`. Arguments are
Linux paths (`/mnt/c/...`). For a Windows MCP client, call WSL directly:

```json
{
  "command": "wsl.exe",
  "args": ["--distribution", "Ubuntu", "--exec", "sh", "-c", "exec ~/.local/bin/phux-mcp"]
}
```

## Desktop app

The desktop app is [Phux Rune](../clients/rune/README.md) (alpha). It needs
macOS 14 on Apple silicon, or Linux with glibc 2.39, Vulkan, and
`libxkbcommon-x11`. It updates itself.

```sh
curl -fsSL https://phux.sh/install | sh -s -- --rune
```

It installs the CLI only when none is found. [Clients](./clients.md#desktop)
says what it does.

## Containers

[`examples/container/`](../examples/container/Dockerfile) has a Dockerfile
and a one-replica Kubernetes StatefulSet.

```sh
docker build -t phux examples/container
docker run -d --init --name phux -v phux-state:/home/phux/.local/state/phux \
  -v phux-config:/home/phux/.config/phux phux
docker exec -it phux phux
```

`--init` reaps pane processes. The two volumes keep sessions and config, so
after a restart `phux restore` starts their programs again.

## Update

```sh
phux update            # install the latest release; panes keep running
phux update --check    # report only
phux update --rollback # put the previous version back
phux channel next      # follow green main; phux channel stable goes back
```

`phux upgrade` moves the running server onto the installed binary without
stopping panes; run it after `brew upgrade`, `apt upgrade`, or another
package manager. For package-manager installs, `phux update` prints that
manager's command instead.

## Shell completion

```sh
phux completion zsh > ~/.zfunc/_phux   # also bash, fish, elvish, nu, powershell
phux man | man -l -
```

## Build from source

Install the [platform packages](./development.md#platform-packages), then:

```sh
git clone https://github.com/no-phux/phux && cd phux
cargo install --locked --path crates/phux
cargo install --locked --path crates/phux-mcp
```

A binary run from a checkout uses a separate `dev` profile with its own
socket and state, so it never touches your installed server's sessions.
[Development](./development.md) covers the full setup.

## Uninstall

```sh
phux skill remove          # if you ran phux skill setup
phux agent uninstall-claude
phux service uninstall     # if you ran phux service install
phux kill --server         # ends every session
rm ~/.local/bin/phux ~/.local/bin/phux-mcp
rm -rf ~/.config/phux ~/.local/state/phux ~/.local/share/phux
rm -rf ~/Library/Application\ Support/sh.phux   # macOS: phux Server
```

Use `brew uninstall phux`, `sudo apt remove phux`, or `sudo dnf remove phux`
for a package install. Remove `Phux Rune.app` for the desktop app; on
Linux remove `~/.local/bin/phux-rune`, `~/.local/share/phux-rune` and
`~/.local/share/applications/sh.phux.rune.desktop`. On Windows, delete
`phux.ps1` and `phux-mcp.ps1` from `%LOCALAPPDATA%\phux\bin` and take it off
`PATH`. [File locations](./reference/files.md) lists the rest.

## Install errors

- `no phux release for <os>/<arch>`: no build for your platform; see
  [Requirements](#requirements) or [build from source](#build-from-source).
- `checksum mismatch for <file>`: nothing was installed; run the script
  again.
- `phux: command not found`: open a new shell, or add `~/.local/bin` to
  `PATH`.

Anything else: [Troubleshooting](./troubleshooting.md).
