---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-09-30
---

# Troubleshooting and recovery

**TL;DR.** Identify the binary, server, and pane before changing state. Check
configuration before retrying startup, distinguish remote reachability from
authentication, and collect logs before restarting. Reattach after a client
disconnects. After a server dies, workspace restore starts fresh processes;
it cannot revive the old ones.

## Choose the symptom

| Symptom | Start here |
|---|---|
| `phux` is missing or shows an unexpected version | [Binary and installation](#phux-is-not-found-or-the-wrong-version-runs) |
| No shell appears or the server will not start | [Startup](#phux-will-not-start) |
| phux works, but your session is absent | [Socket and profile](#the-server-is-running-but-my-session-is-missing) |
| A setting is rejected or has no effect | [Configuration](#a-config-change-fails-or-does-not-apply) |
| An agent has tools but cannot use your panes | [Agent connection](#an-agent-or-mcp-host-cannot-see-the-server) |
| A remote attach fails | [Remote recovery](#a-remote-host-will-not-connect) |
| The server crashed or restarted | [Workspace recovery](#recover-after-a-server-crash-or-reboot) |
| You need to report a reproducible problem | [Collect evidence](#collect-a-useful-report) |

## phux is not found, or the wrong version runs

1. Run `command -v phux` in the failing shell. No path means an installation
   or `PATH` problem, not a server problem. Follow your
   [installation channel's instructions](./INSTALL.md#supported-install-channels);
   the curl installer prints a `PATH` remedy when needed.
2. Run `phux --version` and compare the resolved path with your intended
   install. An old Cargo binary or version-manager shim can shadow a newer
   install. Correct `PATH` ordering; do not copy binaries over another
   installation. Refresh the shell's command cache if necessary.
3. Update through the install's owner. `phux update` does not overwrite
   Homebrew or Nix installs; see the [update procedure](./INSTALL.md#updating).
4. If the binary is correct but the running server is older, run `phux doctor`
   and follow the [version-skew procedure](./operations.md#upgrades-and-version-skew).

**Success:** the intended binary runs and diagnostics no longer report
unexpected server skew. Protocol and package versions are different numbers.

## phux will not start

1. Run interactive `phux` from a real terminal, not a pipe or redirected
   subprocess. For non-interactive work use the [agent CLI](./consumers/agents.md).
2. In another terminal, run `phux status`. If it reports a running server,
   do not restart it: check [session selection](#the-server-is-running-but-my-session-is-missing).
3. If no server is running, run `phux config check`. Fix the named file,
   dotted key, or parse error, then run it again. A missing config uses defaults;
   a malformed config refuses startup rather than silently discarding settings.
4. Retry `phux`. If it still fails, run `phux doctor` and
   `phux logs --server`. Use the reported failing check or log error as the next
   step; repeated restarts do not fix bad configuration or service paths.

**Success:** `phux status` reports a running server and interactive attach
shows a shell. For repeated unexpected restarts, read
[crash-loop visibility](./operations.md#restart-policy-and-crash-loop-visibility)
and [collect a report](#collect-a-useful-report) before changing the service.

## The server is running but my session is missing

1. Run `phux ls`, `phux status`, and `phux doctor` in both the working and
   failing terminal environments. Compare their results before creating a
   replacement session.
2. Check for an explicit `--socket`, inherited `PHUX_SOCKET`, or a different
   `PHUX_PROFILE`. A development build deliberately uses separate state.
   [Profiles and socket precedence](./operations.md#instance-isolation-profiles)
   explain which server each command reaches.
3. Once both clients address the intended server, use `phux attach NAME`
   with the session name from `phux ls`.
4. If the same server no longer lists the session, distinguish an exited pane
   from a server restart using its logs. Reattaching cannot revive a process
   that ended; use [workspace recovery](#recover-after-a-server-crash-or-reboot)
   only if you have an archive.

Do not delete socket files or override development isolation to make the
inventories match. **Success:** you can identify the same session in both views.

## A config change fails or does not apply

1. Run `phux config path` to locate the actual user file, then
   `phux config check` to validate the merged configuration.
2. Run `phux config show --layers` if the effective value is unexpected.
   Another layer may supply it, and assigning an array replaces the inherited
   array unless you use the documented append form.
3. After validation succeeds, run `phux config reload`. The file is not watched.
   A rejected reload leaves the previous client configuration in effect.
4. If the value still does not change, check
   [which settings require reattach or server start](./CONFIG.md#applying-changes).
   Do not restart a server full of live work merely to try a client-side setting.

**Success:** the effective value has the expected source and the relevant
client or newly started server uses it. Follow the
[configuration examples](./CONFIG.md#three-concrete-examples) for a small first edit.

## An agent or MCP host cannot see the server

1. Run `phux status` and `phux ls` from the environment that launches the host.
   If these fail, fix [startup](#phux-will-not-start) or
   [socket selection](#the-server-is-running-but-my-session-is-missing) first.
2. Confirm the integration is loaded. For MCP, `phux mcp --schema` prints the
   installed tool catalog without starting a server; it does not test a live
   connection. `phux mcp` itself does not auto-start one either.
3. Ensure the host passes the intended `PATH` and, for a non-default server,
   `PHUX_SOCKET` to the adapter. For MCP, a per-call `socket` takes precedence;
   the [MCP guide](./consumers/mcp.md#registering-with-a-host) owns the configuration.
4. Ask for an inventory, then a snapshot of one identified pane. If a saved
   target is stale, choose a live one again. Do not bypass a refused shell or
   target check with force just to complete onboarding.
5. An `unsupported_server` refusal on AgentSession commands is a capability
   issue, not evidence that all terminal tools are broken. See
   [agent applicability](./consumers/agents.md#this-tree-older-releases-two-agent-surfaces).

**Success:** the host reads the same intended pane you can see. Resume the
[chosen agent walkthrough](./consumers/getting-started.md) before sending input.

## A remote host will not connect

Follow the [remote diagnostic sequence](./remote-access.md#troubleshooting):
check SSH/server state, the saved route, reachability, firewall admission, then
the actual credential or certificate refusal. Run host-side checks on the
remote host, not only on your laptop.

A timeout is not proof of an overlay failure, and a stalled connection is not
proof of a firewall drop. Keep host firewalls enabled, allow only the intended
binary/port, and never discard a certificate pin to silence a mismatch.
**Success:** `phux attach NAME` opens the intended host's session and a shell
command such as `hostname` confirms where you are.

## Recover after a server crash or reboot

First collect the server log and check [the failure boundary](./operations.md#blast-radius-of-a-panic).
A live update handoff and a fresh server start are not the same operation.

- **The server is still alive:** attach to the existing session. Do not restore
  a workspace merely because one client disconnected.
- **The server died and you have a workspace archive:** start a healthy server,
  then use `phux workspace restore ARCHIVE` with your saved archive path.
  Inspect the command result and inventory; a partial restore names the failed
  sessions and exits non-zero.
- **No archive exists:** create new sessions and recover work through the
  programs' own saved files or native resume mechanisms. phux has no on-disk
  terminal-output journal that can reconstruct the lost processes.

[Workspace save and restore](./operations.md#workspace-continuity-and-update-survival)
preserves layout and startup information for future restarts, not processes
or scrollback.

## Collect a useful report

Run `phux logs` to locate the relevant server/client logs. Then:

```sh
phux report new
phux report show
```

**Expected:** a local bundle and readable report; nothing is uploaded.
[Local bug reports](./operations.md#local-bug-reports) describes its files and
permissions. Review before sharing: logs and screen captures may contain
private work. Include the failing command, expected and observed behavior,
binary version, host/platform, socket/profile, and whether the connection is
local or remote. Never include pairing tokens or private keys.

For latency rather than a connection failure, use
[performance observability](./operations.md#performance-observability) to locate
the slow stage; published [performance measurements](./performance.md) are not
a diagnosis of your machine.
