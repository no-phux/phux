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
      inbound frame → EdgeSession.on_message → send each returned frame.
       Verifies the token; self-closes on idle / hard-max; releases its cap slot.

mode=native → PhuxSessionContainer (src/native-session.ts), one random instance
   └─ worker/Dockerfile: baseline-built phux 0.0.3 → loopback WS → socat on 0.0.0.0:8080
      Separate HTTP readiness on port 8082; no probe bytes reach the WS server.
      No secrets or credentials; Cloudflare `enableInternet = false` blocks egress.
```

The bytes are the **real phux wire** (`phux-protocol`). EdgeSession decodes
`ATTACH` → replies a `TerminalSnapshot` (the shell's greeting), and `InputKey` →
runs the keystroke through the curated shell → `TerminalOutput` (VT bytes). See
`../INFRA.md`.

## Files

| File                | Role                                                                                              |
| ------------------- | ------------------------------------------------------------------------------------------------- |
| `wrangler.jsonc`    | Worker + DO bindings + migrations + tuning vars + the CompiledWasm rule                           |
| `src/index.ts`      | Worker front door: routing, rate-limit, global-cap reserve, token mint, handoff                   |
| `src/global-cap.ts` | `GlobalCapDO` — global concurrency counter + per-IP rate limiter (self-healing via alarm)         |
| `src/session.ts`    | `SessionDO` — instantiates phux-edge WASM, pipes the WS, idle/hard-max close, cap release         |
| `src/token.ts`      | short-lived HMAC-SHA256 session token mint/verify                                                 |
| `src/native-session.ts` | native container lifecycle, hard expiry, cap release, destruction                         |
| `src/native-routing.ts` | native mode parsing, reservation TTL, sanitized upstream request                          |
| `Dockerfile` / `container/` | pinned non-root native image and real interactive Bash profile                         |
| `edge/`             | the built phux-edge WASM artifact (committed; source in `../edge/`, rebuild `bun run build:edge`) |

## Lifecycle / guardrails (server-side)

- **Per-IP rate limit** + **global concurrency cap** at the Worker (excess → a
  "demo at capacity" close, never a queue).
- **Idle close** (2 min) + **hard-max** (10 min) in the SessionDO so an abandoned
  tab doesn't pin a DO. Generous, because a session costs ~nothing (no container).
- Native sessions allow one active shell and two launches per minute per IP.
  They have a 5-minute hard maximum, sleep 15 seconds after the WebSocket
  disconnects, use a soft cap of four, and are bounded by `max_instances = 5`. Their
  global-cap reservation lasts through hard expiry plus launch margin.
- Startup is limited to eight seconds. Disabled, circuit-open, capacity-full,
  startup-error/timeout, and non-101 requests atomically downgrade to edge.
- The strongly consistent circuit defaults to three backend failures/60s,
  60s open, and one half-open probe. Capacity rejection does not count.
- `NATIVE_ENABLED=false` is a deploy-time kill switch. `/healthz` reports only
  aggregate counts and circuit state; there is no public control endpoint.

## Deploy

Pushed automatically by `.github/workflows/deploy-worker.yml` on relevant
worker/package/smoke changes. Native mode requires Workers Paid with Containers
enabled and Docker available to Wrangler. Manually: set the `SESSION_TOKEN_SECRET` secret once
(`bunx wrangler secret put SESSION_TOKEN_SECRET`), then `bun run worker:deploy`.
CI uses the exact phux wire and allowed Origin to verify native greeting, command
execution, and graceful edge fallback. A six-hour synthetic verifies native
independently; manual dispatch supports on-demand probes and fallback exercises.
See `../DEPLOY.md` for kill switch and version rollback.
The edge mode needs no Docker; native deployment builds `Dockerfile`. See
`../DEPLOY.md`.
Optional GitHub App bindings and least-privilege setup are also documented in
`../DEPLOY.md`; its private key is a Worker secret and is never committed or
passed through GitHub Actions.

## Local dev

```sh
bun run dev:worker     # wrangler dev → Worker + DOs at :8787
# worker/.dev.vars:  SESSION_TOKEN_SECRET = "anything"
#                    ALLOWED_ORIGINS = "http://localhost:4321"
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
