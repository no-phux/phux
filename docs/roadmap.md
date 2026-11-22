# Roadmap

What we are building, in the order we expect to build it. There are no dates:
an item moves up when the one before it ships. The list is short on purpose
and is edited by hand, so it shows direction, not every open task.

To ask for something or argue for a different order, open an issue at
[no-phux/phux](https://github.com/no-phux/phux/issues).

## Now

- **Keyboard-first terminal.** Copy mode with vi keys in the TUI and in
  Rune has shipped. Rebindable copy-mode keys and picking a link from the
  screen by keyboard come next.
- **One config for every client.** Rune reads a `[rune]` table from
  `config.toml`. Rune key bindings from the same file come next.
- **Organizing many sessions.** Rune's sidebar groups sessions by project
  and worktree, and the TUI and Rune create a worktree with its session.
  Reopening a closed tab and recent-first ordering come next.

## Next

- **Reach a host with no VPN and no account.** A local client that attaches
  through plain `ssh` when UDP is blocked, including jump hosts, and a
  hosted relay so a phone away from home connects without a VPN.
- **Sessions that come back.** An opt-in mode that restores panes and
  resumes agents after a reboot or a server crash. It never submits input
  to a resumed agent.
- **Android for everyone.** Open testing on Google Play and a signed APK on
  each GitHub release.
- **Rune on Wayland.** Rendering, clipboard, input methods, and fractional
  scaling qualified on current Linux desktops.

## Later

- Port and unix-socket forwarding over the phux connection, including the
  ssh agent.
- A file and git diff view in Rune.
- Download a file from a host to a phone, and attach from the camera or the
  share sheet.
- A static Linux server build and a container image.
- Terminal themes on the phone, pulled from the paired host.
- A push notification when an agent finishes, not only when it needs you.
- Move a dormant agent session to another host in one command.

## Not planned

- **A native Windows server or app.** Windows runs phux in WSL 2; see
  [Windows (WSL)](./install.md#windows-wsl).
- **Moving a running process between hosts.** A program stays on the
  machine that started it. Moving a session means stopping it on one host
  and resuming it on another.
