---
audience: humans
stability: evolving
last-reviewed: 2026-10-22
---

# Desktop updates

**Settings > Updates** checks the channel's release manifest
([release contract](../../docs/releasing.md)) and the phux CLI's own release
at launch and every six hours without blocking terminal input. Turn
**Check automatically** off or use **Check now** at any time. The page shows
both products, installed and incoming versions side by side, and one button
that moves whatever is due. A compact top-right card offers the same actions
without opening Settings. Dismissing it does not cancel an update; the titlebar
indicator reopens it. The card never takes keyboard focus from your terminal.

**Download Desktop** prepares a combined update: it downloads the desktop
bundle and verifies its checksum and signature beside `Phux.app`. The local
server stays on the running Desktop's protocol until you choose **Restart &
update server**, which updates phux with the installed CLI's checksum-verified
updater, hands the live server off, and restarts Desktop only if the handoff
succeeds. The circular indicator shows the archive's real download percentage
when its size is known, then spins through verification. Server work and transfers
of unknown size use an indeterminate indicator, never a made-up percentage.
Animations stop with reduced motion or **Animations** off. For a desktop-only
update, **Restart now** quits, swaps the staged bundle in after the app has
exited, and reopens it. Downloading never automatically restarts Desktop.
Sessions keep running throughout. Either restart reopens your windows on the
same hosts and sessions, with their tabs, splits and focused panes, and brings
the foreground window back to the front. Explicit launch targets take priority
over this one-time restoration.

When only one product is due the button reads **Download update** or
**Update server**; a server-only update needs no desktop restart. If the swap
fails, the previous app is moved back. A downloaded bundle that has not been
swapped in yet offers **Restart now** when the server is already current;
quitting normally also installs a desktop-only update. If a managed server
update or handoff is still pending, quitting normally leaves the installed
Desktop unchanged: use **Restart & update server** to finish both together. The
helper logs to `~/Library/Logs/Phux/update.log`. Quitting the desktop detaches;
it does not stop your server-owned terminals.

**Update phux with Desktop** is on by default. The handoff never silently
closes agent sessions; a refused handoff keeps the downloaded desktop, says
why, and offers **Retry & restart** (or **Retry server handoff** for a server-only
update) without downloading phux again. Homebrew, Cargo and Nix installations
show their native update command instead of replacing package-managed files.
Turning it off leaves the CLI and running server untouched, and the page warns
when a desktop update changes the wire protocol phux speaks. Only this computer's
phux is updated, not remote hosts. Linux Desktop updates rerun the installer;
the card's **Installation instructions** opens the details in Settings.

Choose **Stable** (the latest published release) or **Next** (verified main
builds) for each product under **Release channels**. You can switch back to
Stable even from a newer Next build.

Update preferences live in `$XDG_CONFIG_HOME/phux-desktop/updates.json` (default
`~/.config/phux-desktop/updates.json`). Bare development executables and launches
with explicit CLI/socket/profile overrides cannot install updates.
