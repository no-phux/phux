---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# Enrolled loopback transport proof

**TL;DR.** Run the real native FFI and headless Cockpit Phux provider against
private, TLS-and-token-authenticated QUIC and WSS servers. This is repeatable
transport/lifecycle evidence for `phux-2jza.10`, not completion of the genuine
remote-host Cockpit journey in `phux-c2td.17`.

## Run

From the repository root, using the [native prerequisites](../../../../docs/SETUP.md):

```sh
bash scripts/doctor.sh cockpit
python3 clients/cockpit/scripts/everyday-remote-live.py
```

The default runs both transports and the production provider. `--transport
quic` or `--transport wss` narrows the network lane. `--ffi-only` is an explicitly
scoped inner loop that does not verify the Zig provider. Both real liveness and
history-lease deadlines are exercised, so allow several minutes after building.
Each run builds the same-checkout library and CLI and rebuilds the headless
provider. `--profile ffi-dev` is the default;
`--profile ffi-release` selects that archive consistently for both probes.
Build environments remove `PHUX_CLIENT_FFI_INCLUDE_DIR`,
`PHUX_CLIENT_FFI_LIB_DIR`, `CARGO_TARGET_DIR` and `CARGO_BUILD_TARGET`. The runner
reads the native host from `rustc -vV`, then explicitly supplies Cargo
`--target <host> --target-dir <checkout>/target` for both library and CLI.
This overrides foreign **and same-host** `build.target` settings; the only accepted
output directory is `<checkout>/target/<host>/<profile>`. The provider invocation passes
absolute checkout include/archive directories as well as the selected profile;
an existing provider executable never bypasses the build. Builds use Cargo jobs
2 and Zig `-j2`. The provider build is opt-in and is not run by ordinary
`zig build test`.

A successful run prints `PROVENANCE: /path/to/everyday-remote-provenance-*.json`.
That retained, secret-free record contains the checkout revision, complete
tracked/unignored source-tree and working-diff SHA-256 hashes, selected profile,
the explicit native target, and hashes/paths for the header, archive, CLI,
C probe and provider executable.
Source and artifact identities must remain unchanged through verification.
Private credentials are removed; the provenance JSON stays under the scratch
root. The C probe's recorded path is transient, but its hash remains evidence.

## What is exercised

- `phux pair --json` creates private credentials and the server certificate.
  A private `[[remote]]` registry supplies the token file and certificate pin
  to both the CLI and native tunnel. `whoami --remote` must identify the minted
  credential and a bearer-authenticated route. This is local pairing and
  registration, not SSH enrollment onto another machine.
- The C probe uses `phux_remote_tunnel_*` and `phux_client_*` from the real static
  archive. It reuses the framing pattern of `crates/phux-client-ffi/tests/detach_live.c`;
  no alternate QUIC/WSS implementation or echo-server substitute is introduced.
- The PTY child disables echo and appends each received input line to a file.
  Expected complete file contents establish execution count and destination
  independently of local echo or rendered terminal text. The complete second
  terminal record must equal `["second-terminal-only"]` after all input,
  reconnect and provider operations; a count-only match cannot pass.
- The history fixture emits 1,600 numbered lines. Native history pages load
  while input is dispatched; a scrolled viewport retains its first row during
  imports. The oldest line starts unsearchable and becomes searchable after
  explicit history loading. The negotiated request uses a 32-row hint and
  production-sized byte limits; native structural pages are not synthetic rows.
- Spawn, shared-layout split and window rename go through the public FFI and
  confirmed server metadata. A fresh kernel reconnects to the same server and
  terminal IDs, retaining the renamed split layout. Detached input is refused
  and leaves no queued frame.
- Suspending only the private server with SIGSTOP makes the actual QUIC idle
  timer or WSS ping/liveness timer detect an unresponsive peer. The fixture
  always resumes it with SIGCONT, including on failure. Reconnecting resumes
  the same terminal and layout. This is a silent-peer failure, not a physical
  network-interface or firewall outage.
- Holding a queued native history request beyond the server's actual 30-second
  lease produces `HISTORY_UNAVAILABLE_EXPIRED`; loading settles and terminal
  input continues. No forged history error is fed to the client.
- A formerly accepted credential is revoked in the private store; the native
  tunnel must refuse it. A wrong certificate pin must also fail, with a reason
  available when the embedder sees EOF. Neither dial rewrites the registry.
- A cold server has a different `HELLO_OK` identity and no recreated session.
- The Zig executable uses the production `PhuxProvider`, `Host`, socket worker
  and remote tunnel. It checks real input, retained canvas during reconnect,
  the same terminal reference after readiness, and old-owner input refusal.

## Isolation and evidence boundaries

Every server gets private HOME/XDG directories, a relative private UDS, ephemeral
loopback listeners, credentials and registry, all below the supplied scratch
root (default `/private/tmp/opencode`). Both `--quic` and `--listen` are explicit:
omitting either allows the server's overlay auto-discovery to choose a nonlocal
listener, even when `PHUX_TAILSCALE` names a nonexistent executable. The runner
does not change a user's registry or launch Cockpit. Cleanup reaps its servers
and removes private data. Pairing secrets are not printed.

SIGTERM, SIGHUP and interrupt requests unwind the fixture contexts. Each owned
server/probe is continued before termination, then killed on a bounded timeout
and reaped if necessary. Repeated termination requests cannot interrupt that
cleanup. Child adoption also fences the signal-delivery window around `Popen`;
children receive the original signal mask before executing.

| Contract area | Evidence provided here | Separate acceptance still required |
|---|---|---|
| Product B7–14 | Shared registry resolution, authenticated QUIC/WSS, revoked token and stale pin refusal, registry immutability | Machines UI discovery, truthful per-machine states, setup/cancel/retry, user identity disambiguation, capacity, disconnect/forget interaction |
| Product B21 | Silent-peer detection, disconnected input refusal, same-work reconnect, cold server identity, native provider canvas/owner fencing | Visible recovery presentation, relaunch restoration, input already in flight at fault time and uncertain-delivery messaging |
| Product B34 | Real PTY exact-target execution, history paging/search/pinning, input during history | Native keyboard/IME, selection, mouse/TUI, drag/resize, on-glass responsiveness and rendering |
| `phux-c2td.17` | Authenticated loopback transport and native provider foundations | Actual Cockpit against a genuine enrolled remote machine over QUIC and WSS: connect, split, rename, relaunch reconnect and link drop |

An empty user remote registry supplies no real-host evidence. Keep
`phux-c2td.17` open until that last row is actually driven. Successful loopback
runs do not establish overlay routing, DNS, SSH setup, remote OS behavior,
real-network loss/reordering, or the complete native app journey.

## Recorded validation

On 2026-09-12, on the Apple-silicon development host, against implementation
base `3eeba41f` and its same-checkout `ffi-dev` artifacts:

- `bash scripts/doctor.sh cockpit`: passed, zero prerequisite problems.
- Artifact build with `CARGO_BUILD_JOBS=2`: passed (Phux 0.31.0).
- `everyday-remote-provider` build with `-j2`: passed.
- `everyday-remote-live.py --transport quic`: passed, including the real
  provider test. Authentication route was `bearer-quic`.
- `everyday-remote-live.py --transport wss`: passed, including the real
  provider test. Authentication route was `bearer-wss`.
- Default `everyday-remote-live.py`: passed again with both transports and
  both production-provider invocations in one isolated run.
- Each transport imported eight native history pages and 1,603 rows in the
  history/input assertion. Real 30-second liveness and history-expiry paths
  completed; wrong pins and revoked credentials failed admission.
- `bash scripts/check-docs.sh`, Zig formatting, C compilation with
  `-Wall -Wextra -Werror`, and `git diff --check`: passed.

The initial live scenarios are contract probes. The real
revoked-token run did uncover follow-up `phux-2jza.11`: QUIC reports only
`the connection was lost: connection lost`, while WSS reports `401 Unauthorized`.
Admission is correctly refused on both; preserving an actionable QUIC
authentication reason needs production work in its owning lane.

Complexity was measured with `radon cc -s -a` for the Python runner and
`lizard` for both C files. All new functions had no prior implementation:
Python maximum 9 (`exercise`), C maximum 6 (`main`), other C functions 1–5.
Manual Zig counts: `add` 1, `tick` 1, `ready` 4, `recorded` 1, `input` 3,
provider test 3. The one touched existing function, `addPhuxGraphTests`,
remains 3 before and after the additive build hook (one conditional, one loop).

### Fresh-review regressions

```sh
python3 clients/cockpit/tests/everyday-remote/test_runner.py
```

These tests run disposable mock executables, not a Phux server or native app.
Against the actual `bff8ddb0` runner, the named regressions failed as follows:

| Test | Observed RED | Verified correction |
|---|---|---|
| `test_main_rebuilds_provider_with_pinned_checkout_inputs` | Existing stale provider skipped the expected rebuild: zero builds, expected one | Both `ffi-dev` and `ffi-release` rebuild with explicit checkout paths, remove inherited overrides, and record matching artifact hashes |
| `test_build_overrides_foreign_and_same_host_cargo_target_configuration` | The profile-only helper invocation did not explicitly select a native Cargo target | Native `--target` and `--target-dir` are supplied for both Cargo builds; artifact selection uses the matching target directory |
| `test_termination_during_server_stop_resumes_and_reaps_owned_children` | SIGTERM and SIGHUP left a ps-confirmed stopped server and live probe | Both signals unwind and reap exact owned children; the SIGHUP case also verifies SIGKILL fallback for a mock server refusing SIGTERM |
| `test_wrong_single_secondary_execution_is_rejected` | A single `WRONG-PAYLOAD` line passed | Complete secondary payload mismatch is rejected |

The regressions pass after the review fixes; missing `rustc` host output also
refuses the build. Python complexity before/after
(`radon cc -s -a`): `run` 3→3, `stop` 2→2, `fixture_server` 4→3,
`exercise_stall` 3→2, `exercise` 9→8, `compile_probe` 2→2, `main` 8→1.
Extracted `verify` is 6; signal ownership, build selection and provenance helpers
are individually bounded to 1–4. C and Zig functions are unchanged by this fix.
