---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux file locations reference

**TL;DR.** Where phux keeps its files: the socket under `$XDG_RUNTIME_DIR` (or `/tmp`), config under `$XDG_CONFIG_HOME`, and logs, TLS material, and pairing tokens under `$XDG_STATE_HOME`. Runtime and state paths carry the active profile, so a development build never shares them with an installed one. Each rule is pinned to the resolving function by a unit test, so the page moves when the code does.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

phux splits its files across the three XDG base directories: config (hand-written, follows you between machines), runtime (expected to disappear on reboot), and state (survives across runs but is not config). Paths below are symbolic; each rule names its fallback when the environment variable is unset.

## Profiles

Every runtime and state path carries the active **profile**, so a development build cannot touch the installed build's socket, sessions, or logs (ADR-0080). The profile is `$PHUX_PROFILE` when set; otherwise `dev` for a binary that is not an installed release (a debug build, or one under a Cargo `target/` directory); otherwise `default`.

The `default` profile is unsuffixed — `phux` — so paths created by earlier releases stay valid. Any other profile suffixes the directory name: `phux-dev`, `phux-<name>`. Below, `<profile-dir>` means `phux` or `phux-<profile>` accordingly. `phux doctor` reports the resolved profile.

## Socket

The Unix domain socket every consumer dials and the server binds:

1. `$PHUX_SOCKET` if set (an explicit `--socket` flag still overrides it);
2. `$XDG_RUNTIME_DIR/<profile-dir>/phux.sock` if `XDG_RUNTIME_DIR` is set;
3. `/tmp/phux-<user>[-<profile>]/phux.sock` otherwise.

The parent directory is created mode `0700`. Its sibling `spawn.lock` serialises auto-spawn so concurrent invocations elect one server rather than racing to bind.

A socket file whose server has died is *stale*, not a server: liveness is established by connecting, never by the file existing. A stale entry is removed and a server started, so an uncleanly killed server cannot wedge later invocations.

## Config

`$XDG_CONFIG_HOME/phux/config.toml`, falling back to `~/.config/phux/config.toml` when `XDG_CONFIG_HOME` is unset. A missing config file is not an error (the embedded defaults apply); there is no global config-path flag — set `XDG_CONFIG_HOME` to isolate configuration for a test or an alternate environment. `phux config path` prints the resolved path.

## State

The state directory is `$XDG_STATE_HOME/<profile-dir>`, falling back to `~/.local/state/<profile-dir>` when `XDG_STATE_HOME` is unset or empty:

```
$XDG_STATE_HOME/<profile-dir>/
├── server.log          # the ONE server log, both spawn paths
├── server.log.1..4     # older generations, rotated while live
├── server-starts.log   # one line per server start (crash-loop check)
├── client-<pid>.log    # per-pid interactive-client log
├── onboarding.json     # versioned first-use journey progress
├── onboarding.lock     # serializes first-use moment delivery
├── remote-cert.pem     # auto-provisioned remote-consumer certificate
├── remote-key.pem      # its private key (owner-only, 0600)
└── remote-tokens       # structured credential store (owner-only, 0600)
```

- `server.log` is the canonical server log regardless of how the server was started: the auto-spawn path redirects the daemon's stderr here, and the service unit points its log capture at the same file. `phux logs` and `phux service logs` read it; `PHUX_LOG` tees the server's structured log to an additional file without moving this one. Whenever it exceeds 8 MiB it is rolled aside to `server.log.1` (older generations shifting to `.2`, `.3`, `.4`, oldest dropped), checked at server start and again periodically for as long as the server runs -- so one very long-lived server is bounded the same as many short-lived ones. Rotation truncates the live file in place rather than replacing it, so a `tail -f` or the OS-redirected stdio a service-managed server writes through keeps working across it.
- `server-starts.log` records `<epoch> <pid> <version>` per server start. `phux doctor` counts recent entries to report a crash-loop, and compares the newest version against the running binary's to detect an upgrade the server has not picked up.
- `client-<pid>.log` is where an interactive client writes its trace — the TUI owns the alt screen, so the client never logs to stderr. `PHUX_LOG` redirects it. Log files are created mode `0600`.
- `onboarding.json` records only the versioned first-use journey stage. `onboarding.lock` serializes delivery within that profile. State is best-effort: missing state starts the guidance, while unreadable, unknown, or unwritable state stays quiet and never prevents attach.
- `remote-cert.pem` / `remote-key.pem` are the self-signed TLS pair auto-provisioned for remote consumers (ADR-0031); `PHUX_WS_TLS_CERT` / `PHUX_WS_TLS_KEY` substitute an operator-supplied pair. A complete pair is never regenerated, so the pinned fingerprint stays stable -- which also means its subjectAltName set is fixed at generation (ADR-0091); `phux doctor` reports whether it names the address phux advertises.
- `remote-tokens` is the versioned verifier-only credential store the server reads and `phux pair`, `phux pair rotate`, and `phux pair revoke` update under the sibling `.remote-tokens.lock`. Writers first lock the owner-controlled, non-group/world-writable parent directory, then no-follow open and validate the owner-only regular lock file, preventing lock-path replacement from splitting concurrent writers. Store commits use a synced temporary file and atomic rename. The store must be a regular, non-symlink file owned by the effective user with no group/world permissions; an integrity failure denies authentication. `PHUX_WS_TOKENS` moves it without weakening those checks. Legacy anonymous token lines require the idempotent `phux pair --migrate-legacy` conversion.

## Design intent (not yet implemented)

A `server.pid` file and a `journal/` directory of per-pane PTY output for crash recovery remain design intent; neither path exists today. Workspace archives are written only where `phux workspace save` is pointed.
