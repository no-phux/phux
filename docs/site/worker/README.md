# worker/ — phux live-demo Worker, Durable Objects, and native shell

The sole public door to the live demo. `demo` and `portfolio` run as phux-edge
WASM in `SessionDO`. Production `native` mode starts one disposable
Cloudflare Container with the released native phux server and a real Bash PTY.
Native failures and overflow transparently use an honest `native-fallback`
portfolio shell in `SessionDO` over the same public request.

```
Browser (phux-web wasm: real wire client + libghostty-vt engine)
   │  wss://…/session   ── the ONLY public surface
   ▼
phux-demo Worker  (src/index.ts)
   │  rate-limit (per IP) → reserve global slot → mint session token
   ▼
SessionDO  (src/session.ts)   one per session, a plain Durable Object
   └─ instantiates phux-edge (worker/edge/, WASM) and pipes the WebSocket:
      inbound frame → EdgeSession.on_message → route each returned frame.
       Verifies the token; self-closes on idle / hard-max; releases its cap slot.
       One session holds SEVERAL views: see "Pairing a phone" below.

   wss://…/session?attach=CODE  ── a paired second screen
   └─ verify the attach code (attaching reserves nothing) → the same SessionDO;
      attachment frames go to the view that attached, session frames broadcast,
      and only the newest view may resize the shared terminal.

   GET /session/pair?code=…     ── has this code been spent yet? (pairing panel)

mode=native → PhuxSessionContainer (src/native-session.ts), one random instance
   └─ worker/Dockerfile: pinned workspace phux (ARG PHUX_VERSION + PHUX_REVISION,
      checksummed source, `phux --version` must match) → loopback WS → socat on 0.0.0.0:8080
      Separate HTTP readiness on port 8082; no probe bytes reach the WS server.
      No secrets or credentials; Cloudflare `enableInternet = false` blocks egress.
```

## Pairing a phone

The hosted shell is one session with several views, which is the product's own
model: `ATTACH`/`ATTACHED`/`ATTACH_READY` carry a client-chosen correlation id and
the server allocates a client id per attachment. The landing's dialog hands the
view that opened the session one **attach code**; a phone scans it, opens
`https://phux.sh/phone#CODE`, and attaches to the *same* session — no install, no
sign-in, nothing to press.

- The code is minted by the session DO, delivered only in that view's session
  envelope, and is a signed `scope: "attach"` token (src/token.ts). It cannot open
  a session, and a session token cannot attach.
- It is **single-use** (claimed inside the DO's single-threaded window), lives at
  most five minutes, and never outlives its session.
- Attaching **reserves nothing**: no global-cap slot, no native admission, no
  launch or daily budget. The session's own slot, idle clock, and hard expiry
  remain the only limits.
- Frames are routed per attachment where the wire says so and broadcast
  otherwise, because every view looks at the same terminals. Frames that answer
  one *connection* — `HELLO_OK`, `ERROR`, `PONG`, `DETACHED`, a `COMMAND_RESULT`
  keyed by the requester's id — go only to that view; broadcasting a handshake
  reply is a protocol error to a client that already greeted. Bootstrap streams
  stay broadcast: they declare the replica generation each view's output is
  tagged with, so a view that misses one stops accepting that terminal's output.
  That terminal has one geometry and `attach` applies the attaching client's
  viewport, so **only the newest view may resize it** (src/attach-fanout.ts).
  That is the tmux rule, and it keeps a phone from being handed a shell wider
  than its screen.
- A paired view leaving, or timing out on its own idle clock, detaches that view
  only. The session ends when the view that opened it does.
- The code rides in the pairing page's URL **fragment**, so it never reaches a
  server log or a referrer. The websocket upgrade that spends it carries it in
  the query string, which the demo Worker's invocation logs do see: the same
  bearer-in-URL trade the CLI's `phux pair` link makes, bounded by single-use,
  five minutes, and attach-only.
- Pairing covers the edge shell (`demo`, `portfolio`, `native-fallback`). Native
  sessions refuse an attach until that path grows the same offering.

The bytes are the **real phux wire** (`phux-protocol` 0.9). EdgeSession answers
`HELLO` with `HELLO_OK`, then `ATTACH` with READY-fenced bootstrap (greeting in
`BootstrapChunk`) and `InputKey` with `ResourceOutput` (VT bytes). See
`../INFRA.md`.

## Files

| File                | Role                                                                                              |
| ------------------- | ------------------------------------------------------------------------------------------------- |
| `../infra/demo.run.ts` | Worker + DO bindings + container + tuning vars (Alchemy; WASM is uploaded as a compiled module) |
| `src/index.ts`      | Worker front door: routing, rate-limit, global-cap reserve, token mint, handoff                   |
| `src/global-cap.ts` | `GlobalCapDO` — global concurrency counter + per-IP rate limiter (self-healing via alarm)         |
| `src/session.ts`    | `SessionDO` — instantiates phux-edge WASM, fans out the session's views, idle/hard-max close, cap release |
| `src/attach-fanout.ts` | view registry: per-attachment routing, newest-view geometry rule             |
| `src/wire.ts`       | the three frame families the fan-out must read (attach ids), nothing more     |
| `src/pairing.ts`    | the single-use pairing-code rules, as pure functions                          |
| `src/token.ts`      | short-lived HMAC-SHA256 session token mint/verify                                                 |
| `src/native-session.ts` | native container lifecycle, hard expiry, cap release, destruction                         |
| `src/native-routing.ts` | native mode parsing, reservation TTL, sanitized upstream request                          |
| `Dockerfile` / `container/` | pinned non-root native image (`PHUX_VERSION` / `PHUX_REVISION`) and Bash profile      |
| `edge/`             | the built phux-edge WASM artifact (committed; source in `../edge/`, rebuild `bun run build:edge`) |

## Lifecycle / guardrails (server-side)

- **Per-IP rate limit** + **global concurrency cap** at the Worker (excess → a
  "demo at capacity" close, never a queue). A paired view passes the rate limit
  and nothing else.
- **Idle close** (2 min per view) + **hard-max** (10 min) in the SessionDO so an
  abandoned tab doesn't pin a DO. Generous, because a session costs ~nothing (no
  container). Each attached view carries its own idle clock; only the view that
  opened the session ends it.
- Native sessions allow one active shell and two launches per minute per IP.
  They have a 5-minute hard maximum, sleep 15 seconds after the WebSocket
  disconnects, use a soft cap of four, and are bounded by `max_instances = 5`. Their
  global-cap reservation lasts through hard expiry plus launch margin.
- Startup is limited to eight seconds. Disabled, circuit-open, capacity-full,
  startup-error/timeout, and non-101 requests atomically downgrade to edge.
- The strongly consistent circuit defaults to three backend failures/60s,
  60s open, and one half-open probe. Capacity rejection does not count.
- `NATIVE_ENABLED=false` parks native admission; it is the current default
  (an unset value means off, so an ordinary deploy cannot re-enable it). The
  edge fallback stays live and clients report "Native shells are paused."
  Re-enable deliberately: `gh variable set PHUX_NATIVE_ENABLED --body true`
  (the worker deploy passes it through), or dispatch site-native-control.yml.
  `/healthz` reports only aggregate counts and circuit state, including
  `nativeEnabled`; there is no public control endpoint.

## Native image pin

`Dockerfile` builds phux from a checksummed GitHub archive (`PHUX_REVISION`)
and records the crate version as `PHUX_VERSION`. The final image fails if
`phux --version` is not exactly `phux $PHUX_VERSION`. `just toolchain-check`
and `bun test worker/src` require `PHUX_VERSION` to match
`workspace.package.version`, Zig/libghostty pins to match the repo manifests,
and the `--version` assertion to stay in the Dockerfile. The release job updates
the revision, archive checksum, and lockfile hashes together; the required
workflow gate verifies the pinned source's actual version and lockfile before
merge. See [release versioning](../../RELEASING.md#versioning) for the refresh
command and source-commit ordering. Zig/libghostty/Ghostty ARGs must stay
coherent with that phux revision.

## Deploy

Pushed automatically by `.github/workflows/site-deploy-worker.yml` on relevant
worker/infra/package/smoke changes. Native mode requires Workers Paid with Containers
enabled and Docker available to Alchemy. Manually: set the `SESSION_TOKEN_SECRET` secret once
(`bunx wrangler secret put SESSION_TOKEN_SECRET --name phux-demo`), then `bun run worker:deploy`
(the `phux` Alchemy profile; see `../infra/README.md`).
CI uses the exact phux wire and allowed Origin to verify native greeting, command
execution, and graceful edge fallback. A six-hour synthetic verifies native
independently; manual dispatch supports on-demand probes and fallback exercises.
`bun run worker:smoke-pair` verifies the pairing surface end to end — one session,
two attached views, input on one showing up on the other, the code spending once,
and an attachment reserving no slot — and runs after every worker deploy and in
the six-hour monitor, because pairing is the anonymous demo's front door.
See `../DEPLOY.md` for kill switch and version rollback.
The edge mode needs no Docker; native deployment builds `Dockerfile`. See
`../DEPLOY.md`.
Optional GitHub App bindings and least-privilege setup are also documented in
`../DEPLOY.md`; its private key is a Worker secret and is never committed or
passed through GitHub Actions.

## Local dev

```sh
bun run dev:worker     # alchemy dev → Worker + DOs (edge only) at :8787
# optional infra/.env: SESSION_TOKEN_SECRET=…  (dev values otherwise)
```

Build and test only the native image:

```sh
docker buildx build --platform linux/amd64 --load -t phux-site-native:test worker
docker run --rm --platform linux/amd64 -p 127.0.0.1:8080:8080 \
  --read-only --tmpfs /tmp:rw,noexec,nosuid,nodev,size=64m,uid=10001,gid=10001 \
  --security-opt no-new-privileges --pids-limit 64 --memory 384m --cpus .5 \
  phux-site-native:test
# connect the browser client or phux protocol tooling to ws://127.0.0.1:8080/
```

The production Worker URL is `wss://<worker-host>/session?mode=native`, selected
by `src/pages/embed.astro`. The public request remains native during fallback;
the Worker chooses the backend without reconnecting or changing client protocol.
