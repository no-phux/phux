---
audience: contributors
stability: stable
last-reviewed: 2026-08-07
---

# 0073 — Login-shell semantics for service-managed pane spawns

**TL;DR.** A server started by `phux service install`'s generated unit
stamps `PHUX_SERVICE_MANAGED` into the unit's own environment; `phux
server` reads it back and, only then, invokes the resolved shell in its
platform login mode (`-l` for bash/zsh/sh, `--login` for fish) for every
command-less pane spawn. An ordinary hand-started server never sees the
marker and stays a plain shell, exactly as before.

Status: Accepted
Date: 2026-08-07

## Context

ADR-0055 gave `phux service install` a generated launchd `LaunchAgent` /
systemd user unit so a self-hosted server survives logout and reboot.
Both init systems start that unit with a minimal environment: no login
shell ever ran, so the `PATH` additions Homebrew's or Nix's installer put
in `~/.zprofile` / `~/.profile` never took effect. Every pane the server
spawns inherits that minimal `PATH`; `nvim` and `brew` report "command
not found" even though the user's ordinary interactive shell has them.
Environment markers such as `NIX_PROFILES` can still be inherited from
whatever built the unit, so a guard some profile script uses to skip
re-initialization ("have I already run?") can be fooled into thinking
it already ran when it never has — ruling out "just re-source the
profile unconditionally" as a fix.

The natural remedy — spawn the pane's shell in login mode so it sources
the same profile scripts an interactive terminal login would — has one
hazard that makes it wrong as a blanket default: a server a human started
directly from their own terminal (`phux server`, or the naked auto-spawn
path) already runs inside a fully profile-initialized environment.
Re-running login-shell initialization there is not idempotent for every
setup — PATH duplication is the mild failure, `nvm`/`rbenv`/`direnv`
guards misfiring is not. Login-shell treatment therefore has to be
conditional on *how the server itself was started*, and that condition
has to be something the server knows, not something it guesses: sniffing
"my `PATH` looks short" or "my parent is launchd" is exactly the kind of
heuristic the `NIX_PROFILES` problem above already shows is unreliable in
both directions.

## Decision

1. **The installer stamps a marker into the unit it generates.**
   `ServicePlan::environment()` (`crates/phux/src/commands/service.rs`)
   unconditionally adds `PHUX_SERVICE_MANAGED=1` to both the launchd
   `EnvironmentVariables` dict and the systemd `Environment=` lines. This
   is not a heuristic: a server without it was never started from a unit
   this `phux` wrote, full stop. A value this code itself writes at
   install time, read back by the same code family at startup, has no
   ambiguity a sniffed signal can have.
2. **`phux server` reads the marker once, at startup.** `run_server`
   (`crates/phux/src/commands/server.rs`) checks
   `PHUX_SERVICE_MANAGED`'s presence and threads the resulting `bool`
   through `phux_server::runtime::ServerConfig::login_shell`, mirrored
   into `ServerState` exactly like the existing `shell` field
   (`phux-i0e8.4.1`'s precedent), and consumed at every command-less pane
   spawn site (the pre-seeded session, `--seed-command`/
   `spawn-on-attach`, attach-time `CreateIfMissing`,
   `SESSION_CREATE_KEY`, a command-less `SPAWN_TERMINAL`).
3. **Login mode is a per-shell argv flag, looked up by basename, applied
   only for recognized shells:**

   | shell        | flag       |
   |--------------|------------|
   | `bash`       | `-l`       |
   | `zsh`        | `-l`       |
   | `fish`       | `--login`  |
   | `sh`         | `-l`       |

   `sh` is included because `/bin/sh` is the documented last-resort
   fallback (`resolve_shell`): macOS ships it as bash's `sh`
   personality, Linux almost always as `dash`; both read `/etc/profile`
   then `~/.profile` under `-l`, non-interactively included. `fish` takes
   `--login`, not `-l`. A `defaults.shell` this table does not recognize
   — a custom shell, a wrapper script, a typo — gets **no login flag at
   all**, even when the server is service-managed: an unrecognized
   program has unknown flag semantics, and hard-failing every pane spawn
   on an unrecognized shell is a worse regression than a pane whose
   profile never ran. `login_flag_for_shell`
   (`crates/phux-server/src/terminal_actor/spawn.rs`) is the single
   source of truth for this table.
4. **The installer never captures its own transient `PATH`.**
   `ServicePlan::environment()` never reads `PATH` at all — resolving it
   was already unnecessary before this decision — so a unit generated
   from inside a `nix develop` or direnv shell freezes none of that
   shell's `PATH` into the unit. The init system's own `PATH` reaches the
   server process unmodified; login-shell treatment is how the *pane*
   recovers the profile's `PATH`, not a baked-in snapshot of the
   installer's.

## Why

- A self-written marker is the only signal in this problem that is
  actually reliable. Every environment-shape heuristic considered
  (`PATH` length, parent pid, presence of `NIX_PROFILES`) is defeated by
  the same fact that motivates this ADR: a service-managed server can
  still inherit profile-shaped environment markers from whatever built
  its unit, and an ordinary terminal-launched server can have an
  unusually short `PATH` for reasons that have nothing to do with how it
  was started.
- Per-shell flags, not a single blanket `-l`, because `-l` is not
  universal — fish's login flag is a distinct word, and blindly passing
  `-l` to an unknown program risks a fatal exec rather than a merely
  incomplete environment.
- Threading `login_shell` through the exact same mirror-into-`ServerState`
  pattern `shell` already uses (rather than inventing a second channel)
  keeps every spawn site's precedence identical and auditable in one
  place per phux-i0e8.4.1's precedent.

## Tradeoffs

- Rerunning `phux service install` is required to pick up this fix on an
  already-running service-managed server: the marker is not present in a
  unit generated before this ADR, and the server only reads it at its own
  startup.
- Only four shell families are recognized. An operator on a shell with
  its own login-mode spelling not in the table (e.g. a very old `ksh`
  variant, or `nushell`) gets a spawn that behaves exactly as it did
  before this ADR — not a regression, but not a fix either — until the
  table gains an entry.
- The marker lives in the unit's own environment, which means it also
  survives a graceful upgrade re-exec (ADR-0032) — the resumed image
  reads the same environment, so this needs no separate handoff wiring,
  but it also means an operator who manually copies the unit's
  `EnvironmentVariables`/`Environment=` block into a hand-started
  `phux server` invocation would (correctly, if surprisingly) get
  login-shell panes too.

## Alternatives

- **Sniff environment shape (short `PATH`, parent is launchd/systemd,
  absence of an interactive tty)** — rejected: this is precisely the
  class of heuristic the bug report's `NIX_PROFILES` observation shows
  is unreliable in both directions.
- **Bake the installer's own `PATH` into the generated unit** — rejected:
  a `phux service install` run from inside `nix develop` or direnv would
  freeze that shell's transient `PATH` into the unit forever, going stale
  the moment the installing shell exits, and shadowing whatever
  login-shell `PATH` a pane would otherwise resolve.
- **Always spawn every pane's shell in login mode, unconditionally** —
  rejected: an ordinary terminal-launched server's environment is already
  login-shell-initialized, and re-running profile scripts a second time
  is not idempotent for every setup.
- **A `defaults.login-shell` config knob the operator sets by hand** —
  rejected as the primary mechanism: it demands the operator diagnose
  *why* `nvim` is missing before they can fix it, which is the exact
  discovery cost this ADR removes. Nothing here precludes adding one
  later as an override if a real setup needs to force the mode either
  way.
