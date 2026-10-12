// phux-demo: the live-demo Worker — the SOLE public door to the demo.
//
//   Browser WS ──► this Worker ──► SessionDO (demo/portfolio, edge WASM)
//                              └─► PhuxSessionContainer (native mode)
//
// The Worker enforces per-IP rate limits and concurrency caps before either
// backend starts. SessionDO remains the free OS-less fallback; native mode runs
// one credential-free, no-egress container per visitor.
//
// Deployed by .github/workflows/site-deploy-worker.yml (and the native kill
// switch, site-native-control.yml). Paths are relative to this directory.
// Like alchemy.run.ts, CI starts from empty state and re-adopts the Worker by
// its alchemy:* tags; the container application is adopted by name.
import * as Alchemy from "alchemy";
import { adopt } from "alchemy/AdoptPolicy";
import * as Cloudflare from "alchemy/Cloudflare";
import { retain } from "alchemy/RemovalPolicy";
import * as State from "alchemy/State";
import { Stack } from "alchemy/Stack";
import * as Effect from "effect/Effect";
import { requirePhuxAccount } from "./account.ts";

export const PRODUCTION_STAGE = "prod";

/**
 * Native admission kill switch. Every deploy sets it; site-native-control.yml
 * redeploys with PHUX_NATIVE_ENABLED=false to contain native shells while the
 * edge fallback stays live. The next ordinary deploy turns native back on.
 */
export const nativeEnabled = (value = process.env.PHUX_NATIVE_ENABLED) => {
  if (value === undefined || value === "") return "true";
  if (value !== "true" && value !== "false") {
    throw new Error(`PHUX_NATIVE_ENABLED must be "true" or "false" (got "${value}")`);
  }
  return value;
};

export default Alchemy.Stack(
  "phux-demo",
  { providers: Cloudflare.providers(), state: State.localState() },
  Effect.gen(function* () {
    const { stage } = yield* Stack;
    if (stage !== PRODUCTION_STAGE) {
      return yield* Effect.die(
        new Error(
          `phux-demo only deploys stage "${PRODUCTION_STAGE}" (got "${stage}"); every resource is a production singleton.`,
        ),
      );
    }
    yield* requirePhuxAccount;

    const demo = yield* Cloudflare.Worker("Demo", {
      name: "phux-demo",
      main: "../worker/src/index.ts",
      // The modern DO + WebSocket surface require a recent compat date. Client
      // disconnects explicitly complete the close handshake (INFRA.md); the
      // pinned 2025 date does not auto-reply.
      compatibility: { date: "2025-05-01", flags: ["nodejs_compat"] },
      // Stable first-party endpoint for auth callbacks, health, and terminal WSS.
      domain: "shell.phux.sh",
      // Browser OAuth must not navigate to shell.phux.sh: Cloudflare 403s some
      // document requests to that hostname before they reach this Worker.
      // Only /auth/ paths belong here — narrowing keeps /auth.md (the site's
      // agent-auth document) on the static site worker.
      // Pinned by id: the deploy token has Workers Routes Edit on phux.sh, not
      // Zone Read for a by-name lookup.
      routes: [
        { pattern: "phux.sh/auth/*", zoneId: "efe8d7a4ec74ca0561c8915a77714f93" },
      ],
      workersDev: false,
      observability: {
        enabled: true,
        logs: { enabled: true, invocationLogs: true, persist: true },
      },
      env: {
        // SessionDO: one per live session; runs the phux-edge WASM server
        // (../worker/edge/*_bg.wasm, uploaded as a compiled WASM module).
        SESSION: Cloudflare.DurableObject("SESSION", { className: "SessionDO" }),
        // GlobalCapDO: atomic global/native/account admission.
        GLOBAL_CAP: Cloudflare.DurableObject("GLOBAL_CAP", {
          className: "GlobalCapDO",
        }),
        // RateLimitDO: one secret-keyed object per IP sliding window.
        RATE_LIMIT: Cloudflare.DurableObject("RATE_LIMIT", {
          className: "RateLimitDO",
        }),
        // Native shells: one container per visitor behind PhuxSessionContainer.
        // `name` is the application Wrangler created, so it is adopted, not
        // duplicated. PHUX_VERSION in the Dockerfile is kept current by the
        // release sync.
        PHUX_SESSION: Cloudflare.Container("PHUX_SESSION", {
          name: "phux-demo-phuxsessioncontainer",
          className: "PhuxSessionContainer",
          context: "../worker",
          instanceType: "lite",
          maxInstances: 30,
          // Replace running shells in steps on image changes, as Wrangler did.
          rollout: { strategy: "rolling", stepPercentage: 25 },
        }),

        // ── Vars ────────────────────────────────────────────────────────────
        // Non-secret tuning knobs. Secrets (SESSION_TOKEN_SECRET,
        // AUTH_COOKIE_SECRET, OAuth client ids/secrets, MEMBER_KEY,
        // SYNTHETIC_TOKEN_SECRET, TELEMETRY_INGEST_KEY, optional GITHUB_APP_*)
        // are put on the Worker (../DEPLOY.md) and kept across deploys.
        IDLE_KILL_MS: "120000", // 2 min of no input → close
        HARD_MAX_MS: "600000", // 10 min wall-clock cap → close
        NATIVE_HARD_MAX_MS: "300000", // native shell wall-clock cap: 5 min
        NATIVE_ENABLED: nativeEnabled(), // false routes native requests to edge
        NATIVE_CONCURRENCY_CAP: "25", // soft cap; platform hard max remains 30
        NATIVE_PER_IP_CAP: "1", // active native shell per source IP
        NATIVE_RATE_LIMIT_PER_MIN: "2", // native launches per IP per minute
        NATIVE_PER_ACCOUNT_CAP: "1", // active native shell per provider identity
        NATIVE_LAUNCHES_PER_HOUR: "6", // rolling native launches per identity
        NATIVE_DAILY_MS: "1800000", // 30 native minutes per identity per UTC day
        NATIVE_STARTUP_TIMEOUT_MS: "8000",
        NATIVE_CIRCUIT_FAILURE_THRESHOLD: "3",
        NATIVE_CIRCUIT_WINDOW_MS: "60000",
        NATIVE_CIRCUIT_OPEN_MS: "60000",
        GLOBAL_CONCURRENCY_CAP: "100", // live sessions allowed at once, globally
        RATE_LIMIT_PER_MIN: "10", // new sessions per IP per 60s window
        SESSION_TTL_MS: "30000", // session-token validity after minting
        ALLOWED_ORIGINS: "https://phux.sh",
        // Where demo session telemetry is forwarded (site worker ingest route).
        TELEMETRY_INGEST_URL: "https://phux.sh/api/telemetry/ingest",
      },
    }).pipe(adopt(true), retain());

    // Private analytics stays off the public network: a same-account service
    // binding to the Worker that no-phux/ops deploys.
    yield* demo.bind("Analytics", {
      bindings: [
        { type: "service", name: "ANALYTICS", service: "phux-ops-analytics" },
      ],
    });

    return { worker: demo.workerName, urls: demo.urls };
  }),
);
