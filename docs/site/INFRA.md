# phux-site — live demo infrastructure spec

How the ▶ live phux terminal on the homepage works. All infra code (terminal
island, the edge server, Worker/DO) builds against this. If the code and this
file disagree, fix this file first.

See `CONTENT.md` for messaging; this file is purely the demo backend.

---

## The shape

```
Browser (phux-web wasm: the real wire client + embedded libghostty-vt engine)
   │  WebSocket, hosted session envelope + binary phux wire frames
   ▼
Cloudflare Worker  (worker/)
   │  signed OAuth session, identity/IP quotas, global cap, native circuit breaker
   │  routes to native when healthy, otherwise →
Durable Object  (worker/, one per session)
   └─ runs phux-edge (WASM): the real phux wire codec + a curated portfolio shell.
       No OS, no processes, no container — free on the edge.

`?mode=native` requires a verified GitHub or Google session and normally routes to a fresh
`PhuxSessionContainer`. Native phux listens plaintext on loopback; a byte-transparent
socat process exposes port 8080 to the platform. A separate HTTP-only port 8082
provides startup readiness without sending probes into the WebSocket server. The
binary phux protocol is unchanged. Disabled, circuit-open, over-soft-cap, timed-out,
failed, and non-101 starts atomically downgrade the reservation and receive the
edge portfolio shell over the same WebSocket request instead of a close or queue.
```

The production `demo`/`portfolio` path is **free**: the phux _server_ runs as WASM inside the Durable
Object, so there's no container (no Workers Paid). The browser client is
unchanged — it speaks the same phux wire whether the server is real native phux,
a jailed container, or this edge WASM server.

Idle edge sockets use Durable Object WebSocket hibernation. The object persists
only a versioned logical shell checkpoint (viewport, sequence, partial demo
input, or portfolio selection), then reconstructs the WASM session on the next
message. One alarm enforces idle and hard deadlines; there are no JavaScript
timers or reservation heartbeats keeping the object resident.

The native path requires Workers Paid and Cloudflare Containers and is selected
by `/embed`. The edge path remains the instant graceful fallback.

## The wire contract

Every hosted socket begins with one deployment envelope, followed by the
**phux wire protocol** directly with no reimplementation.

- The server's first message is the text `phux.session.v1` control object. It is
  emitted only after backend selection is final and carries the actual `edge` or
  `native` backend, authoritative expiry timestamp, and an optional fallback
  reason from a closed safe vocabulary. Raw provider, container, and upstream
  errors never enter it.

- **WS BINARY frame = one length-prefixed phux `FrameKind`, both directions** —
  the exact codec native phux uses. The browser runs `phux-web` (real
  `phux-protocol` + libghostty-vt engine); the DO runs `phux-edge` (real
  `phux-protocol` + a curated shell that emits VT bytes). `TerminalSnapshot` /
  `TerminalOutput` carry VT bytes; the client's engine renders them.
- After that envelope, the Worker/DO never reframes binary data. The edge DO _is_
  the server (decode frame → shell → encode frame); the native container DO
  relays binary frames byte-for-byte.
- **Close** is app-private `4xxx` (idle, max-lifetime, cap, unauthorized).

phux-edge is `edge/` (Rust→WASM), with its built artifact committed into `worker/edge/`.

## Lifecycle / guardrails (server-side defaults)

A session is ~free (a DO holding a WebSocket), so the limits are generous,
purely anti-abuse:

- **Per-IP rate limit** in secret-keyed per-address Durable Objects
  (`RATE_LIMIT_PER_MIN`; native shells use
  the stricter `NATIVE_RATE_LIMIT_PER_MIN`).
- **Global concurrency cap** (`GLOBAL_CONCURRENCY_CAP`) — excess gets a "demo at
  capacity" close, never a queue.
- **Idle close** (`IDLE_KILL_MS`, 2 min) and **hard-max** (`HARD_MAX_MS`, 10 min)
  so an abandoned tab doesn't pin a DO.
- **Native:** verified `provider:subject` identity, one active shell per account,
  six launches per rolling hour, 30 minutes per UTC day, one active shell per IP,
  and two launches per IP per minute. `NATIVE_HARD_MAX_MS` is five minutes,
  containers sleep 15 seconds after disconnect, the admission soft cap is 25,
  and the platform cap is 30 `lite` instances. The five extra platform slots are
  operational headroom. The global reservation includes the hard maximum plus a
  launch margin.
- Native admission never queues. Native overflow receives an edge 101 response.
  The total edge/native session cap still applies.
- `GlobalCapDO` owns only atomic global, native, and account admission; IP
  windows are sharded so unrelated clients cannot serialize on that object.
- `GlobalCapDO` opens its strongly consistent native circuit after three
  startup/pre-upgrade failures in 60 seconds by default. It remains open for 60
  seconds, then permits exactly one half-open probe. Capacity fallback is not a
  backend failure. A successful start resets the circuit.
- `NATIVE_ENABLED` defaults true. False skips native allocation and serves the
  honest edge fallback. It is a deploy-time Worker variable, not a public API.

## The edge shell (phux-edge)

A curated, OS-less interpreter (`edge/src/shell.rs`): line editing + a small
command set (`help`, `ls`/`cat` over an in-memory FS, `echo`, `pwd`, `clear`,
`demo` / `demo links`) that emits VT bytes — truecolor, OSC 8 hyperlinks, the
phux logo. Not arbitrary execution (no `/bin/sh`), which is also why it's safe:
there's no process to break out of.

## The native shell

`worker/Dockerfile` builds a reproducible linux/amd64 image from digest-pinned
Debian, Rust, and Bun bases. Phux 0.0.3 is pinned to commit
`1f2501f979be972886a1adb805bb598b9189e2f9`; its source archive and lockfile are
verified, and Rust plus libghostty's Zig build target the conservative x86-64
GNU/Linux baseline. Ghui 0.10.0 is pinned to commit
`81ddf0a00b73e522394181278ec78cce133b57d0` with its source and lockfile
verified. The phui standalone uses Bun's conservative x64 baseline target. The PTY is
genuine interactive Bash. `phui` runs the actual standalone binary with fixed
`GHUI_MOCK_*` (the pinned pre-rename build still reads them) data and cache/preferences disabled; `phux` is the released binary.
No credentials are injected. The guest has Git, Python/pip, Node, GCC/G++, make,
jq, ripgrep, fd, curl, archives, and standard shell utilities; apt/dpkg, sudo,
SSH, and daemons are removed after image construction. These image controls are
defense in depth; Cloudflare `enableInternet = false` is the enforced no-egress
boundary.

Each random container name is used once. `prepare` binds its session ID,
schedules hard expiry, and releases `GLOBAL_CAP` idempotently on idle, hard
expiry, or process stop before destruction. Allocation failures release the
native portion before the same reservation is downgraded to edge. Late stop
callbacks cannot release that edge slot. Startup is bounded with the Containers
cancellation API (eight seconds by default), and failed allocations are
destroyed. The upstream request is normalized to `/` and strips
mode, client-IP admission headers, and Worker-only session headers.

The fallback greeting explicitly says native is busy or unavailable and labels
itself the instant edge tour. It does not claim to be a native shell or offer a
retry action the client cannot guarantee.

## Availability and diagnostics

- Target SLO: accepted native requests receive either a native or edge 101 within
  10 seconds; native startup has an 8-second budget. No native queue exists.
- `/healthz` returns sanitized live/native counts, kill-switch state, and circuit
  state/failure count. It contains no IPs, session IDs, credentials, or controls.
- A six-hour GitHub Actions synthetic uses the exact binary phux wire, exact
  production Origin, native greeting, and command marker. Manual dispatch remains
  available for an on-demand probe or fallback exercise. A dedicated bearer
  secret authenticates only this non-browser monitor; browser WebSockets cannot
  set the required header. It closes cleanly and never logs terminal contents.

## Env / config contract

- Frontend reads `PUBLIC_PHUX_DEMO_WS` (`src/lib/site.ts`). Empty → "coming
  online" state, never dials. Production uses
  `wss://shell.phux.sh/session`; the workers.dev hostname is not a public
  frontend contract.
- OAuth and WebSockets share `https://shell.phux.sh`, so the host-only
  `Secure`, `HttpOnly`, `SameSite=Lax` session cookie is present on native
  upgrades without exposing a token to browser JavaScript. Session cookies are
  signed and expire after eight hours. GitHub uses numeric user IDs and requires
  seven-day-old accounts; Google uses the signed OIDC `sub` and requires
  `email_verified`. Provider tokens are discarded after callback verification.
- `/embed` sends lifecycle, authoritative backend, expiry, and normalized close
  state only to the exact validated `https://phall.io` parent origin. It never
  sends user identity or raw close/provider details. `live` waits for both the
  session envelope and non-uniform terminal pixels.
- Anonymous visitors see an explicit choice: authenticate with GitHub/Google for
  native Linux, or launch the always-available edge shell. OAuth runs in a popup;
  the callback reports only completion to the same-origin embed, which then
  rechecks the HttpOnly session cookie. The popup never sends identity or tokens.

## File ownership

| Area            | Owns                                                               |
| --------------- | ------------------------------------------------------------------ |
| Docs pipeline   | `scripts/sync-docs.ts`, `src/content/`, doc pages                  |
| Terminal island | `src/components/PhuxTerminal.tsx`, `src/lib/phux-web/` (generated) |
| Edge server     | `edge/**` (Rust), `worker/edge/**` (built artifact)                |
| Worker + DO     | `worker/**`, root `package.json`                                   |

## Public portfolio console

The `portfolio` demo mode discovers public repositories from the `phall1`
account that carry the `phall-showcase` topic. Onboarding and removal do not
require a deploy:

```bash
gh repo edit phall1/new-project --add-topic phall-showcase
gh repo edit phall1/new-project --remove-topic phall-showcase
```

Discovery is cached at the edge for five minutes. The Worker rejects repositories
unless they have the exact owner, public visibility, active state, and showcase
topic. With both GitHub App bindings configured it uses short-lived installation
tokens; with neither it retains unauthenticated public discovery. A partial App
configuration fails closed. The App is installed for all repositories so a new
public repository needs only the topic command above, never a site deploy.

The Worker accepts session upgrades only from the exact origins in
`ALLOWED_ORIGINS`; production permits `https://phux.sh`. For local
development, override that variable in `worker/.dev.vars` rather than widening
the deployed policy.
