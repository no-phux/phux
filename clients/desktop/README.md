# phux desktop

Install the GPUI desktop client on Apple-silicon macOS 27 or later, or Linux
x86-64/ARM64 with X11 or Wayland and Vulkan:

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

That is [the phux installer](https://phux.sh/install) with `--desktop`. It
verifies the ZIP against the channel manifest's digest, checks the app
signature and bundle identity, and installs `Phux.app` plus a `phux-desktop`
launcher. It installs the phux CLI first when none is found, but never
replaces an existing CLI or stops its server. Run it again to update; pin or
roll back with `sh -s -- --version X.Y.Z` after the pipe. `--applications-dir`
and `--install-dir` select alternate destinations. Releases are ad-hoc signed,
**not Apple-notarized**; after verification the installer clears quarantine.

Linux uses a checksummed, signed tarball instead of an Apple bundle. See
[Linux installation](../../docs/INSTALL.md#linux-desktop) and the
[desktop prerequisites](../../docs/SETUP.md#gpui-desktop). Shortcuts use Super
instead of Command; `Ctrl-Shift-P/T/W/F` also open the palette, create/close a
terminal, and find. `Ctrl-Shift-C/V` copy/paste without intercepting shell
`Ctrl-C`. Text fields use normal Linux `Ctrl-C/V` editing shortcuts.
Linux updates are explicit installer reruns; the Updates panel checks releases
and prints the signature-required command rather than offering a macOS swap.

`Phux.app` bundles the `phux-desktop` binary built from `clients/desktop/native`.
There is no JavaScript bundle and no NAPI addon. Launched from the Dock or
Spotlight, the app resolves the installed `phux` CLI
(`~/.local/bin/phux`, `/opt/homebrew/bin/phux`, `/usr/local/bin/phux`, or
`~/.cargo/bin/phux`) and starts or reuses the server. A shell-only `PATH` or
`PHUX_BIN` is not enough for a Dock launch. It opens your Intake, the session
untargeted work lands in; a server without one opens its most recent session.
The app never creates a session on its own. `PHUX_SESSION` names a session
instead and creates it, persistent, when it does not exist; `PHUX_PROFILE` and
`PHUX_SOCKET` pick the server. Startup problems are shown in the window and logged to
`~/Library/Logs/phux-desktop.log` on macOS or
`$XDG_STATE_HOME/phux-desktop/desktop.log` on Linux (default
`~/.local/state/phux-desktop/desktop.log`; redacted, rotated at 8 MiB). **Copy
Diagnostics** in the command palette copies versions, server id, connection
epoch, host and recent errors for a bug report, without terminal content.

The sidebar is the one navigator: every session across This Mac and registered
hosts, grouped by **Project** (repository identity), by **Machine**, or not at
all; the chevron at its top switches the grouping. A session on another machine
names that machine in faint text on its own row; This Mac needs no label, and
offline inventory reads **offline**. The session this window shows opens in
place, listing its tabs (with **Tabs in Sidebar**) and any of its terminals no
tab holds. An agent that needs you marks the tab, terminal or folded session it
is in, rather than a separate list. The titlebar's main menu names the active
session, and its machine when that is not this Mac, even with the sidebar
hidden. Selecting a session restores its layout in this window without moving
or restarting its processes.
Intake and imported sessions remain ordinary work, not a separate navigation root.
Select work and choose
**Organize Selected Work…** to move it to a named session, or explicitly
**Leave unorganized (Intake)**. Moving changes membership, never execution.
Imported sessions can also be marked organized without moving their work.
The bell opens **Inbox & notifications**: agent attention alerts and the
server's held approvals in Inbox, errors and command results in
Notifications. An approval entry offers **Approve** and **Deny** and leaves
the Inbox once the server no longer holds it. Its badge counts unread alerts,
not sessions or messages; an agent alert opens the agent's terminal.
**Organize Everything** (`⌘⇧O`, in the command palette) does it for every
pane at once, the way `phux organize` does: each pane joins its
repository's session, a repository with no session gets one, and a numbered
session holding one repository takes its name. A notification lists what
moved; **Undo Organize Everything** reverses it, as does
`phux organize --undo`.

Closing a pane/window or quitting the app detaches; it does not terminate
server-owned shells. A desktop crash is separate from a server crash:
relaunching can reattach while the server remains alive, but this release
does **not** promise process survival after a server crash or machine reboot.
Only **Terminate Terminal Process** and **Kill Session** intentionally end
terminals.

`just desktop-package` builds the app without installing it. The app's
`Contents/Resources` carries the repository `LICENSE` and `NOTICE` and a
`THIRD-PARTY-NOTICES.txt` generated from the desktop's Cargo graph: every
shipped crate with its declared license, then the license texts the crates
ship. Packaging fails if any shipped crate declares no license.
`just desktop-install-app` builds and installs `/Applications/Phux.app`.
Release ownership and qualification are in [Releasing](../../docs/RELEASING.md).

For development, run from this checkout instead:

```sh
just desktop-app
```

That bootstraps the parent-managed GPUI snapshot, builds the debug desktop
executable and `phux` from this tree, starts a server if one is not already
running, and opens the desktop. Set `PHUX_SOCKET` only when you want a server
other than the one this checkout just ensured. `PHUX_PROFILE=<name>` isolates
the whole instance. `PHUX_DESKTOP_BACKGROUND=1` asks the app not to steal focus.

For a disposable demo that cannot reuse the installed server:

```sh
just desktop-demo
```

This builds the CLI and desktop executable from this checkout, then owns a
private foreground server and app. Each run gets a temporary socket, HOME, and
XDG directories; inherited Phux listener/auth settings are cleared. Closing the
app or interrupting the supervisor reaps both children and removes that state.
The demo refuses installed or external artifacts and does not restart a lost
server behind the supervisor.

## Using it

See the [desktop usage guide](USAGE.md#using-it) for navigation, tab placement,
shortcuts, and settings. Existing section links continue below.

### Command palette

See [Command palette](USAGE.md#command-palette).

### Performance monitor

See [Performance monitor](USAGE.md#performance-monitor).

### Updates

See [Updates](USAGE.md#updates).

### Coming from Ghostty

See [Coming from Ghostty](USAGE.md#coming-from-ghostty).

### Native agents

See [Native agents](USAGE.md#native-agents).

## Qualification

From the repository root:

```sh
just doctor desktop
just desktop-install
just desktop-check
just desktop-framework-check
```

`desktop-check` is the CI gate: script typecheck, lint, qualification-guard
tests, and the desktop host lock comparison. It does not launch GPUI.
The opt-in GPUI journey regressions run real window close/reopen and reconnect
lifecycles against a disposable server started by a checkout-built CLI:

```sh
cargo build -p phux
PHUX_DESKTOP_JOURNEY_TEST_BIN="$PWD/target/debug/phux" \
  XDG_STATE_HOME="$PWD/clients/desktop/.cache/journey-state" \
  cargo test --locked --manifest-path clients/desktop/native/Cargo.toml \
  --lib app::session::journey_tests -- --ignored
```

Remote attach has its own opt-in acceptance run. It enrolls a second,
isolated server as a remote host over loopback QUIC and WSS (`phux pair`,
then `phux host add NAME quic://... --token-file ... --cert-fingerprint ...`)
and drives the window's connection through it: split, type, rename,
reconnect, relaunch by persistent key, a host kill and restart, a stale pin,
and a revoked token, both on the next dial and under a window still attached:

```sh
cargo build -p phux
PHUX_DESKTOP_JOURNEY_TEST_BIN="$PWD/target/debug/phux" \
  cargo test --locked --manifest-path clients/desktop/native/Cargo.toml \
  --lib connection::remote_acceptance -- --ignored
```

It does not replace a run against a second machine: a routed network with
NAT, loss, and real latency, and a host enrolled with `phux host add me@HOST`
over ssh. Do that by hand after a change to remote attach: register the host,
open it from the sidebar's Hosts section, split, rename, quit and relaunch,
and drop the link (sleep the host or cut its network) and bring it back.

`desktop-framework-check` packages `Phux.app` and runs

```sh
bun clients/desktop/scripts/smoke-app.ts --app /absolute/Phux.app --phux /absolute/phux
```

against an isolated same-checkout server. The app must write `presented.png`,
`layout.json`, and `interaction.json` from a drawable-presented frame. The
harness OCRs the PNG and proves the same shell PID survives desktop
SIGKILL/relaunch and SIGTERM/relaunch. A harness-written ok flag is not
success. It does not exercise server crashes, reboot, or restoration of
secondary windows. Missing GPU access is a blocked run.

## Script tooling

From `clients/desktop`, with repository-pinned Bun 1.4.2 and Node 24 on `PATH`:

```sh
export BUN_INSTALL_CACHE_DIR="$PWD/.cache/bun"
bun install --frozen-lockfile
bun run check:tooling
```

Bun runs the scripts. It is not the production host. `check:tooling` formats,
typechecks, and lints those scripts, then runs the vendored rule suites and
the qualification-guard tests. Fix with `bun run format` and
`bun run lint --fix`. Exact tool versions are pinned in `package.json` and
`bun.lock`. A tooling pass is not GPU evidence; see
[desktop verification](../../docs/architecture/desktop-verification.md).

### Boundaries enforced by the gate

- Type-aware unsafe operations, floating and misused promises fail.
- Chained casts, unjustified assertions and module mocks fail via the
  [selected anti-slop rules](tools/oxlint/anti-slop/README.md).
- Script tooling must not import Cockpit or the web clients.
- Negative fixtures are excluded from ordinary lint and run explicitly with
  asserted diagnostics, so an unloaded plugin cannot appear green.
- Qualification tests reject an installed CLI, a production socket, and smoke
  evidence that is only an ok flag.
