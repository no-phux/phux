# Deploying phux-site

Two production deploys provide a native shell with an edge fallback:

| Piece                                                                                                          | Cloudflare product | Cost                                      |
| -------------------------------------------------------------------------------------------------------------- | ------------------ | ----------------------------------------- |
| The static site (`dist/`, incl. the committed wasm client) — a Workers static-assets worker (`wrangler.jsonc`) | **Workers**        | free                                      |
| Edge fallback (`SessionDO` running **phux-edge**) | **Workers / Durable Objects** | low/free-tier usage; no container |
| Production `?mode=native` (`PhuxSessionContainer`) | **Workers Containers** | Workers Paid + `lite` runtime/build storage |

The fallback runs the phux server itself as WASM inside a Durable Object, so it
adds no container runtime cost. Native requires Workers Paid. The site also
works without the backend (`PUBLIC_PHUX_DEMO_WS` empty → the terminal shows
"coming online").

Both deploy on push to `main` via GitHub Actions (below). No manual wrangler.

---

## 1. The site → Workers static assets

A Workers static-assets worker (`phux-site`, config in `wrangler.jsonc` — no
script, assets only). Deploys via `site-deploy.yml` at the phux repo root, or
manually with `bun run deploy`. Build `bun run build`, output `dist`; the wasm
client is committed under `src/lib/phux-web/`, so no Rust/Zig is needed. The
`phux.sh` custom domain is declared in `wrangler.jsonc` and attaches on
deploy.

## 2. The backend → Worker + Durable Objects

`site-deploy-worker.yml` deploys it on relevant worker, package, lockfile, and smoke
script changes. The committed **phux-edge** WASM still needs no Rust build. With
the container binding,
Wrangler also needs a working Docker daemon to build and upload the linux/amd64
image. Do not deploy native configuration until the Cloudflare account is on
Workers Paid and Containers is enabled.

The shell that runs in the DO is `phux-edge` (`edge/`, Rust→WASM). Rebuild it
when that crate changes:

```sh
bun run build:edge        # needs rust 1.90 + wasm-pack (phux nix devshell), → worker/edge/
```

---

## Push-to-deploy (GitHub Actions)

All site workflows live at the phux repo root (`.github/workflows/site-*.yml`);
paths are scoped to `docs/site/**`.

- **`site-deploy.yml`** → site (Workers static assets on phux.sh) on every push
  touching `docs/**` or `ADR/**` (docs and site share one repo, so doc edits
  rebuild the site directly); manual dispatch is available.
- **`site-deploy-worker.yml`** → validates, records the prior version, deploys, then
  exercises native and same-IP edge fallback.
- **`site-native-monitor.yml`** → six-hour exact-wire native synthetic; manual dispatch
  can run an on-demand probe or exercise graceful fallback.
- **`site-native-control.yml`** → private `workflow_dispatch` kill-switch deployment.
- **`site-rollback-worker.yml`** → private version-ID rollback.

### One-time setup

1. **Cloudflare API token** (My Profile → API Tokens) with _Workers Scripts:
   Edit_ **+** _Containers: Write_ (account scope). Grab your **Account ID**.
2. **Repo secrets** (Settings → Secrets and variables → Actions):
   ```
   CLOUDFLARE_API_TOKEN   = <token>
   CLOUDFLARE_ACCOUNT_ID  = <account id>
   ```
3. **Custom domains** attach once (`phux.sh` on the site worker,
   `shell.phux.sh` on the demo worker — both are declared in the wrangler
   configs, so `bun run deploy` / `bun run worker:deploy` attach them; the
   zone must live in the same account).
4. **Session and OAuth secrets** on the Worker (after the first deploy):
   ```sh
   openssl rand -hex 32 | bunx wrangler secret put SESSION_TOKEN_SECRET --cwd worker
   openssl rand -hex 32 | bunx wrangler secret put AUTH_COOKIE_SECRET --cwd worker
   ```
   Generate the production monitor credential once, install the same value in
   the Worker and the repository's `PHUX_SYNTHETIC_TOKEN` Actions secret, then
   discard the local value. It is accepted only as a native WebSocket bearer
   header, which browser WebSockets cannot set.
   ```sh
   monitor_secret="$(openssl rand -hex 32)"
   printf %s "$monitor_secret" | bunx wrangler secret put SYNTHETIC_TOKEN_SECRET --cwd worker
   gh secret set PHUX_SYNTHETIC_TOKEN --body "$monitor_secret"
   unset monitor_secret
   ```
5. **OAuth clients:** register exact callbacks, with no wildcard redirects:
   - GitHub OAuth App: `https://shell.phux.sh/auth/github/callback`
   - Google Web OAuth client: `https://shell.phux.sh/auth/google/callback`

   Install all four provider values atomically from a temporary local file so a
   deployment cannot see a half-configured provider:
   ```sh
   jq -n \
     --arg github_id "$GITHUB_OAUTH_CLIENT_ID" \
     --arg github_secret "$GITHUB_OAUTH_CLIENT_SECRET" \
     --arg google_id "$GOOGLE_OIDC_CLIENT_ID" \
     --arg google_secret "$GOOGLE_OIDC_CLIENT_SECRET" \
     '{GITHUB_OAUTH_CLIENT_ID:$github_id,GITHUB_OAUTH_CLIENT_SECRET:$github_secret,GOOGLE_OIDC_CLIENT_ID:$google_id,GOOGLE_OIDC_CLIENT_SECRET:$google_secret}' \
     | bunx wrangler secret bulk --cwd worker
   ```
   Provider access and ID tokens are used only to verify identity during the
   callback and are never persisted or sent to the browser.
6. **Wire site → Worker:** the Worker owns the `shell.phux.sh` custom
   domain from `worker/wrangler.jsonc`. Set the repo **variable**
   `PUBLIC_PHUX_DEMO_WS = wss://shell.phux.sh/session` and push (or re-run
   `deploy-site`). The build inlines it.
7. **Native prerequisite:** confirm Workers Paid, Containers entitlement, and
   capacity for 30 `lite` instances. Admission uses a 25-session soft cap and
   leaves five instances as platform headroom.

Rotate OAuth client secrets by creating the replacement at the provider,
uploading it with `wrangler secret put`, verifying a complete login, and only
then revoking the old value. Rotating `AUTH_COOKIE_SECRET` intentionally signs
every user out; upload the replacement and verify both providers immediately.

The committed browser artifact is deliberately pinned to the protocol-0.5
hosted-client backport. `scripts/build-client.sh` rejects any other Phux checkout
so a protocol-0.8 client cannot accidentally be deployed against the pinned
native server.

### Optional read-only GitHub App for portfolio discovery

Until this is configured, the Worker deliberately keeps using GitHub's
unauthenticated public repository API. Configure both bindings together: if only
one is present, portfolio discovery fails closed. The private key belongs only in
Cloudflare Worker secrets, not this repository or GitHub Actions.

1. In GitHub, open **Settings → Developer settings → GitHub Apps → New GitHub
   App**. Use any unique name and homepage URL `https://phux.sh`; disable
   the webhook, leave callback URLs empty, and do not request user authorization
   during installation. Choose **Only on this account** for installation scope.
   Set repository permission **Actions: Read-only**. GitHub's
   mandatory **Metadata: Read-only** permission is sufficient for repository
   discovery; grant no other repository, organization, or account permissions.
2. Create the App, note its numeric **App ID**, generate a private key, and
   install it on the `phall1` account for **All repositories**. All-repository
   installation is what lets future public repositories appear without a
   deploy; the exact `phall-showcase` topic remains the publication boundary.
3. Convert the downloaded key to unencrypted PKCS#8. Then upload both bindings
   to Cloudflare in one atomic bulk operation and remove the converted file.
   The App ID is not sensitive, but storing it in the same operation prevents a
   partially configured deployment:

   ```sh
   openssl pkcs8 -topk8 -nocrypt -in ~/Downloads/<app-name>.*.private-key.pem -out /tmp/phux-github-app.pkcs8.pem
   jq -n --arg app_id '<numeric app id>' --rawfile private_key /tmp/phux-github-app.pkcs8.pem \
     '{GITHUB_APP_ID:$app_id,GITHUB_APP_PRIVATE_KEY:$private_key}' \
     | bunx wrangler secret bulk --cwd worker
   rm /tmp/phux-github-app.pkcs8.pem
   ```

4. Verify
   `https://shell.phux.sh/healthz`. The Worker creates a
   short-lived App JWT, resolves the `phall1` installation, and caches only the
   installation token and expiry in isolate memory. Edge snapshot cache entries
   contain only the public display schema.

Onboard or remove any public repository in one command; no deploy is required:

```sh
gh repo edit phall1/<repo> --add-topic phall-showcase
gh repo edit phall1/<repo> --remove-topic phall-showcase
```

After that: **push to `main` → it deploys.** Free.

---

## Lifecycle / abuse guardrails (server-side)

`worker/wrangler.jsonc` + the DOs: secret-keyed per-IP rate-limit objects, a
global concurrency cap,
and the DO self-closes on idle (2 min) / hard-max (10 min). Generous, because a
session is ~free (a DO holding a WebSocket — no container).

Native requires a verified GitHub or Google identity. It uses the same origin
and global admission, plus a soft limit of 25 active containers, 30 `lite`
platform instances, one active shell per account, six launches per rolling hour
per account, 30 minutes per UTC day per account, one active shell per IP, and two
launches per IP per minute. It has a five-minute hard maximum and 15-second
post-disconnect sleep.
No Worker secret is forwarded into the image. `enableInternet = false` is the
Cloudflare-enforced egress control.

Native startup has an eight-second deadline. Failure atomically becomes the edge
portfolio shell, never a queue or closed upgrade. The default circuit opens after
three startup/pre-upgrade failures in 60 seconds, stays open 60 seconds, and
allows one cooldown probe. Capacity fallback does not trip it. `/healthz`
exposes only aggregate diagnostics.

### Kill switch

`NATIVE_ENABLED` defaults to `true`. Run the `native-control` workflow with
`enabled=false` to deploy a Worker version that routes all native requests to
edge. This is intentionally not a public endpoint. Changing a Worker variable
requires deployment, and the next normal main deployment restores the checked-in
value; workflow history is the audit trail.

Use the kill switch, not a pre-auth Worker rollback, for emergency containment.
`site-rollback-worker.yml` requires an explicit confirmation that the target version
preserves authenticated native admission and verifies `/auth/session` after the
rollback. A version from before the auth boundary is not a valid rollback target.

---

## Local dev

```sh
bun run dev:worker        # wrangler dev — the Worker + DOs (edge) at :8787
#   set worker/.dev.vars:  SESSION_TOKEN_SECRET = "anything"
#                         ALLOWED_ORIGINS = "http://localhost:4321"
PUBLIC_PHUX_DEMO_WS=ws://localhost:8787/session bun run dev   # the site, pointed at it
```

(Or `bun run dev:live` to run the **real** native phux server instead of the
edge shell — for developing the client against real phux.)

### Native image

```sh
docker buildx build --platform linux/amd64 --load -t phux-site-native:test worker
docker run --rm --platform linux/amd64 -p 127.0.0.1:8080:8080 \
  --read-only --tmpfs /tmp:rw,noexec,nosuid,nodev,size=64m,uid=10001,gid=10001 \
  --security-opt no-new-privileges --pids-limit 64 --memory 384m --cpus .5 \
  phux-site-native:test
```

Connect protocol tooling to `ws://127.0.0.1:8080/`. Confirm `HELLO`/`ATTACH`,
the greeting, `echo PHUX_NATIVE_OK`, `phui --version`, and a clean restart.

### Smoke, image activation, and rollback

Run the private `native-monitor` workflow with `exercise_fallback=true` before
merging a native image change. It consumes two starts from the runner IP, holds
one native session, verifies the second gets an edge 101 and honest greeting,
then closes both. The scheduled probe uses one start every six hours, below the
two/minute limit. Neither path logs frame contents.

Every deployment records the previous Cloudflare Worker version in its Actions
summary. Roll back that exact version with the private `rollback-worker`
workflow, or one authenticated command:

```sh
bunx wrangler rollback <previous-version-id> --cwd worker --name phux-demo --message "rollback" --yes
```

Cloudflare Versions can split Worker traffic with `wrangler versions deploy`,
but container image rollout and per-IP admission make an unattended canary
unsafe here. There is no automatic rollback. Use the manual pre-activation smoke
and explicit version rollback. No native container data is durable. Never remove
or rewrite historical `v1`/`v2` migrations.
