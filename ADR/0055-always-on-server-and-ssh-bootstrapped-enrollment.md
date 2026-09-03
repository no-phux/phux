---
audience: contributors
stability: stable
last-reviewed: 2026-08-02
---

# 0055 — Always-on server and ssh-bootstrapped enrollment

**TL;DR.** phux generates its own service unit (`launchd` on macOS,
systemd user unit on Linux) so a self-hosted server survives logout and
reboot. A client-side `[[remote]]` registry names a server the way
`[[satellite]]` names a hub peer, and `phux enroll HOST` mints the whole
credential over the operator's existing ssh trust. The human never
transcribes a hex string.

Status: Accepted
Date: 2026-07-25
Amended: 2026-09-02 — the launchd unit's `ProcessType` is `Interactive`, not
`Background`. `Background` asked the scheduler to throttle the one process
whose keystroke echo the user feels; measured under CPU contention it turned a
0.5 ms echo p99 into 15-60 ms with the server near idle. `phux service
reconcile` moves an installed unit over. See ADR-0095 and `docs/operations.md`
§"Scheduling class".

## Context

The remote path works and is unusable. `phux pair` mints a token and
prints a fingerprint, ADR-0031 makes a routable bind fail closed without
both, ADR-0037 hands the operator an overlay address — and then the
operator copies two 64-hex strings by hand into a command line they must
retype on every attach. `phux pair --qr` solved this for phones by
encoding a `phux://connect` deep link; laptops got nothing, because a
laptop has no camera pointed at the server's terminal.

Two gaps sit under that. First, nothing keeps the server running: the
socket auto-spawn in the naked `phux` path covers "socket missing," not
"host rebooted," so a self-hosted mini loses its server on every restart
and the operator must ssh in to resurrect it. Second, there is no
client-side notion of a named server. `[[satellite]]` (ADR-0038) names a
peer a *hub* dials; the *consumer* side has only positional flags, so
`phux attach` cannot resolve `mini` to an endpoint, a pin, and a token.

The unexploited asset is ssh. An operator self-hosting a phux server
already holds ssh trust to that host — they used it to install phux. That
channel is authenticated, confidential, and already accepted by the
operator's threat model, which makes it exactly the right carrier for a
one-time credential bootstrap.

## Decision

1. **phux generates and owns a service unit.** `phux service
   <install|uninstall|status|logs>` writes a `launchd` LaunchAgent on
   macOS and a systemd **user** unit on Linux, with restart-on-exit and
   start-at-boot, the server's env (`PHUX_QUIC_ADDR`, `PHUX_WS_ADDR`,
   `PHUX_WS_TOKENS`, `PHUX_WS_TLS_CERT`) materialized into the unit, and
   logs under the existing state dir. Install is idempotent: rerunning it
   reconciles a changed unit rather than erroring. User-scope only — a
   system-wide unit implies a multi-user server, which ADR-0003's
   one-server-per-user model does not have.
2. **Reboot preserves the server, never the panes.** A restarted service
   is a new server with no terminals; every PTY died with the host. The
   opt-in `--restore` flag wires `phux workspace save` on stop and
   `restore` on start, which brings back session names, layout, and cwd —
   not running processes. It is opt-in because a session list repopulated
   with fresh shells is a surprise, not a feature, unless asked for.
3. **A client-side `[[remote]]` registry names servers.** Each entry:
   `name`, `endpoint` (`quic://`, `wss://`, or `ssh://`), `token-file`
   (0o600, never inline), `cert-fingerprint`. `phux attach NAME` resolves
   an entry to a `Dial` with no flags; explicit `--quic`/`--ws` flags
   still win. The schema deliberately mirrors `[[satellite]]` field for
   field and inherits ADR-0038's fail-closed pin posture, but stays a
   separate table: the trust direction is opposite (consumer to server,
   not hub to peer), and collapsing them would let a hub's peer list and
   a laptop's server list drift into one another's lifecycle.
4. **A registered remote name beats the local-session reading of the same
   word.** `phux attach mini` checks the registry first and dials the
   remote if it hits; only an unregistered name means "local session."
   An entry exists because the operator ran `enroll` or `remote add` for
   it, which is a deliberate statement about what that word means on this
   machine. `--socket` is explicit local intent and suppresses the lookup.
   Registry names may not start with a selector sigil (`@ # . =`) or
   contain `:` or `/`, so the registry can never shadow the selector
   grammar — only a bare session name.
5. **`ssh://` endpoints re-exec ssh instead of dialing.** There is no
   consumer-side ssh transport (`Dial` is QUIC or WebSocket) and there
   need not be: `ssh -t HOST phux attach` puts the session on the remote
   server, where it outlives the connection. This is the zero-ceremony
   rung of the ladder and the honest fallback when a host has nothing
   dialable.
6. **`phux enroll HOST` bootstraps over ssh.** It runs `phux pair --json`
   on the far end through ssh (honoring the `$PHUX_SSH` seam the hub
   dialer already uses), optionally installs the service there, selects an
   overlay address from the pair output, and writes the local `[[remote]]`
   entry plus its token file. `--json` is added to `phux pair` for this;
   the human-readable output is unchanged.
7. **Remote names are local labels, not routes.** A `[[remote]]` name is
   a lookup key in one operator's config. It is not ADR-0052's SNI route
   name, which is a relay-side identity bound at relay enrollment. A later
   `endpoint = "relay://..."` may carry a route name as data; the two
   namespaces stay distinct.

## Why

- ssh is the only bootstrap channel that costs the operator nothing. QR
  needs a camera, manual transcription needs two 64-hex strings, and a
  rendezvous service needs infrastructure ADR-0037 already rejected. The
  trust argument is airtight in the direction that matters: whoever can
  `ssh HOST` can already run `phux pair` there and read the token, so
  enrollment grants no authority ssh did not already grant.
- Generating the unit beats documenting it because the failure mode of a
  documented unit is a subtly wrong one — wrong binary path, missing env,
  system scope instead of user scope — that surfaces as "my sessions
  vanished" days later. The env wiring is the whole point; a hand-written
  plist that omits `PHUX_WS_TOKENS` silently disables remote auth.
- A separate `[[remote]]` table costs one schema and buys an invariant:
  nothing a consumer writes can enable a hub route. Reusing
  `[[satellite]]` would put `phux enroll` in the business of editing the
  federation topology.
- Fingerprint pinning already defeats MITM independent of how the pin
  arrived, so bootstrapping the pin over ssh adds no new trust
  assumption — it removes a transcription error class.

## Tradeoffs

- Two init systems to generate for, and their reload semantics differ
  (`launchctl bootstrap`/`bootout` vs `systemctl --user`). Third
  platforms and non-systemd Linux get a printed unit and a manual
  instruction rather than a bare error: `install --print` renders on every
  platform and exits 0, and `install` proper prints the same unit plus the
  instruction. `install` still exits non-zero there, because nothing was
  installed and `phux service install && …` must not run its right-hand
  side; the promise is that the operator is never left without the
  artifact, not that a no-op reports success (phux-l83y).
- `phux enroll` shells out to ssh and inherits every ssh failure mode
  (agent not loaded, host key prompt, `phux` not on the remote `PATH`)
  into a phux error surface. Mitigated by naming the failing step, never
  a bare exit code.
- `--restore` restores shape without state, which is a genuinely
  half-magic result. Accepted over the alternatives: restoring nothing is
  worse, and restoring processes is not possible.
- Storing tokens as files referenced from config means enrollment writes
  two places and the operator can desynchronize them by hand. The 0o600
  file is still strictly better than an inline secret in `config.toml`.
- The registry is per-machine config with no sync story. An operator with
  three laptops enrolls three times.
- Registry-first name resolution means a local session sharing a name with
  a registered remote becomes unreachable by name. Accepted: the collision
  requires the operator to have deliberately registered that word, and
  `--socket` still reaches the local server. The alternative — a sigil or a
  `--remote` flag — costs the `phux mini` ergonomics that motivate the
  whole feature.

## Alternatives

- **Document a launchd plist in `docs/` and ship no code** — rejected:
  the env wiring is exactly what operators get wrong, and a wrong unit
  fails silently and late.
- **Reuse `[[satellite]]` for consumer remotes** — rejected: opposite
  trust direction, and it would couple a laptop's server list to hub
  federation topology.
- **Extend `phux://connect` deep links to the desktop with a URL handler**
  — rejected for now: it needs OS-level scheme registration on every
  platform to save a step ssh already saves for free. Still the right
  answer for phones, which is why `--qr` stays.
- **A phux-hosted rendezvous that brokers credentials** — rejected:
  ADR-0037 already ruled out hosted relays and rendezvous for
  reachability; the same reasoning applies to credentials, with a
  strictly worse trust story.
- **System-wide service unit** — rejected: implies a multi-user server,
  which ADR-0003 does not have.
