---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-09-09
---

# Reconstructed product specification

**TL;DR.** Superlogical presents persistent terminal sessions across local and
remote hosts through one UI and the Rex CLI. The demonstration exercises remote
login, split panes, naming, CLI-created sessions, reconnect, deletion and
host-aware directory navigation. Requirements below reconstruct demonstrated
behavior; claims about internals and performance retain their evidence limits.

## Product intent and boundary

**N, 04:31–04:47 and 05:27–05:42:** remove the local/remote boundary. The presenter
states that what works locally should work remotely and vice versa. The target
workflow is a developer moving between a Mac and remote Linux infrastructure
without changing terminal interaction habits.

The video calls the application **Superlogical** and its multiplexer **Rex**
(00:00–00:05; 00:59–01:04). Those are separate concepts in this reconstruction.
The local example happens to be in a `ghostty` source directory; that does not
establish that the application shown is a released Ghostty version.

Evidence classes **O/N/M/I/U** are defined in the [package guide](README.md).
`Sxx` references resolve in the [viewer](index.html) and [manifest](frames.json).

## Demonstrated feature inventory

| ID | Feature and reconstructed behavior | Evidence | Class |
|---|---|---|---|
| F01 | Retain local shell state after quitting and reopening the UI; typed `hello` survives. | S01–S03; 00:15–00:25 | O |
| F02 | Search a global command palette and invoke Add Remote Host. | S04–S05; 00:30–00:43 | O |
| F03 | Accept a hostname and connect to a remote terminal using familiar window/tab/pane chrome. | S05–S06 | O |
| F04 | Create side-by-side remote terminal panes with independent shell content/focus. | S07–S08, S20 | O |
| F05 | Carry the remote splits over one connection. | 00:46–00:51 | N |
| F06 | Show Rex sessions in Linux login tooling (`loginctl list-sessions`, later `who`). | S08–S09 | O |
| F07 | Perform a full SSH-style system login: login-shell setup and per-user limits honored. | 00:51–01:32 | N |
| F08 | Group sessions under local/remote hosts and generate human-readable names. | S09, S17; 01:36–01:44 | O; uniqueness guarantee N |
| F09 | Rename the active session through the command palette; reflect `Demo` in header and switcher. | S10–S12 | O |
| F10 | Switch between local and remote sessions using the same selector and keyboard interaction. | S09, S12–S16, S32 | O |
| F11 | Make the Rex CLI available in local and remote shells. | S13–S14; 01:55–02:04 | O; automatic injection N |
| F12 | Query a remote host's identity from the local terminal: principal, authentication route and effective user. | S15; 02:12–02:35 | O |
| F13 | Select root as effective user through a second remote identity query. | S16; 02:35–02:56 | O |
| F14 | Map authenticated principals to allowed effective users on the server and respect existing SSH keys. | 02:41–02:54 | N |
| F15 | Create a remote session from the local CLI and surface it in the graphical selector. | S17; 02:59–03:09 | O |
| F16 | Represent a new session without terminals as an explicit Empty session state, with New Tab action. | S18 | O |
| F17 | Open the first shell tab in that session; other sessions retain their distinct contents. | S19–S20 | O |
| F18 | Reconnect after application quit and return to the last active remote session with prior splits and contents. | S20–S22; 03:55–04:10 | O |
| F19 | Kill a remote session by ID using the local CLI; remove its row from the session selector. | S23–S24; 04:12–04:30 | O |
| F20 | Propagate lifecycle changes quickly to all clients. | 04:26–04:30 | N; one GUI client shown |
| F21 | Invoke Go to Directory, browse a local path and open a new terminal at the destination. | S25–S27; 04:51–05:06 | O |
| F22 | Reuse the directory picker against the remote filesystem; browse `/`, `/proc`, `/home`, then a user directory. | S28–S31; 05:07–05:26 | O |
| F23 | Keep the prior tab/pane layout while the selected directory opens in a new tab. | S27, S31–S32 | O |
| F24 | Deliver responsive remote input using some architectural ideas similar to Mosh. | 03:22–03:55 | N; no quantitative latency claim verified |

## Visible capabilities not exercised

These are part of the recoverable feature surface, but **M** is not evidence of
complete behavior or availability in another build.

### Graphical commands

S04 and S10 show: Change Session, Rename Session, Rename Tab, Switch Host,
Add Remote Host, Disconnect from Host, New Session, New Tab, Go to Directory,
New Window, Close Tab, Previous Session, Next Session, Refresh Sessions,
Close Session, Close Window, Hide Pane Headers, Compact Density, Comfortable
Density, Split Pane Down, Focus Pane Down, Focus Next Pane, and Check for
Updates. The closer rename sequence also reveals Switch to Horizontal Tabs
and Switch to Vertical Tabs. These were visually read from sampled frames;
the full palette can contain more commands outside the visible viewport.

The top-right `+` and per-pane toolbar icons are visible. Their exact hit targets,
tooltips and all glyph meanings cannot be recovered from this encoding. Do not
assign arbitrary actions to each icon as if they were observed.

### CLI help

S13 shows `rex [command] [flags]`, with common commands including `attach`,
`events`, `kill`, `list`/`ls`, `new`, `run`, `send`, and `split`; resources named
`block`, `session`, and `window`; server-related commands `login-server`,
`serve`, `server`, `whoami`; and `completion`, `help`, `version`.
Help text describes event streaming, session listing, command execution,
block input, window splitting and server management. Autostart, color,
server-endpoint and verbosity options are visible; exact flag spellings and
defaults should be read from the original source or a higher-resolution capture
before implementing compatibility. No block-management UX is demonstrated.

## CLI interaction contract recovered from the demo

These are descriptive command shapes, **not instructions to run against the
presenter's infrastructure**. Hostnames and opaque identifiers are placeholders.
The original literal text remains in screenshots.

```text
rex
rex -s <host> whoami
rex -s root@<host> whoami
rex -s <host> session new
rex -s <host> session kill session:<opaque-id>
```

**O:** creation prints a session identifier; the GUI shows a generated display
name. **I:** stable identity and mutable display name should be distinct in the
reconstructed model. Rename must not change the identity used for deletion.
The video does not exercise exit statuses, JSON output, invalid selectors,
conflict handling or automation guarantees.

## Reconstructed domain model

```text
Desktop application/window
  active host + active session reference
  session switcher grouped by host

Host (local or remote)
  Sessions (stable ID, display name)
    Tabs/windows [UI/CLI terminology correspondence not proven]
      Pane layout
        Terminal/shell (working directory, output, input, focus)

Authenticated principal --server authorization--> Effective OS user
CLI and GUI -------------------------------> Shared session state
```

**I:** window state is a client projection of longer-lived sessions. The demo
does not reveal whether split geometry lives in the server, local preferences
or both. “Block” appears in CLI help, but its exact relationship to a pane or
terminal is **U**. Avoid making that an asserted entity mapping.

## State and lifecycle requirements

| Entity | Demonstrated states/transitions | Unresolved states |
|---|---|---|
| Application | Open → quit → reopen → prior local/remote content | Crash recovery, version upgrade, restore opt-out |
| Host | Hostname entry → connected; host appears in selector | DNS/auth failure, unreachable, expired identity, removed host |
| Session | Created empty → first tab; rename; switch; killed → absent | Active-session kill, last-tab close, ID reuse, duplicate names |
| Pane | Shell → input/output; two independently focused splits | Resize policy, nested splits, zoom, process exit |
| Directory picker | Current path → entries → chosen destination → new terminal | Missing path, permission denied, symlinks, files, slow listings |

Quitting the app and killing a session are demonstrably different operations.
**I:** any reconstruction must preserve that distinction. No retention period,
server restart durability or offline resumption guarantee is established.

## Reconstructed acceptance scenarios

These define how a future implementation could reproduce the demo, not tests
that were run against Superlogical or phux.

1. **Local persistence (F01):** type an unsubmitted marker; quit and reopen the
   app; the same marker and shell context remain.
2. **Remote equivalence (F02–F05):** connect to an authorized host via the palette;
   create a split; both shells run on the selected host, with independent input.
   Verify shared transport separately from visible output.
3. **Session naming (F08–F10):** rename the remote session; both header and selector
   update; switch to local and back without losing pane state.
4. **CLI/GUI coherence (F11–F17):** create a session through a local CLI targeting
   the remote host; observe it in the GUI; select its empty state; open a tab.
5. **Reconnect (F18):** populate both remote panes differently; quit and reopen;
   restore the last selected session, geometry, output and working directories.
6. **Lifecycle propagation (F19–F20):** kill a non-active remote session by ID;
   its selector entry disappears, while the active session stays intact. A
   separate multi-client test would be needed to establish “all clients.”
7. **Directory parity (F21–F23):** invoke the same shortcut locally and remotely;
   verify listings originate from the active host; choose a path; get a new
   terminal on that host at that path while the original tab remains available.
8. **Login semantics (F06–F07, F12–F14):** inspect OS login records and effective
   user; separately test shell initialization, limits and authorization. A root
   identity query alone does not prove an interactive root shell was opened.

## Unknowns and non-claims

- **U:** protocol framing, transport choice, encryption, discovery, credential
  storage, Tailscale dependency, reconnect algorithm and prediction technique.
  The presenter expressly defers the protocol explanation (03:47–03:55).
- **U:** whether arbitrary SSH servers are supported. “SSH replacement” is a
  narrated positioning claim, not proof of SSH wire compatibility.
- **U:** other platforms, pricing, licensing, release date, configuration, agent
  integration, collaboration, file transfer and richer terminal rendering.
- **U:** actual round-trip latency, session propagation timing and reliability
  during packet loss. A recorded keystroke overlay is not a benchmark clock.
- **I:** loading, failure and accessibility states in the companion UX document
  are necessary design completions, not recovered product behavior.

## Design implications for terminal research

The transferable design is host-aware *context*, not merely a remote shell
command: the session selector, creation commands, directory picker and restored
tabs all operate on the same host. Session existence is independent from open
tabs, and session control is shared between GUI and CLI. These are useful inputs
to phux design discussion, but no phux API mapping or implementation decision is
made by this research package.
