// phux-site: the static site Worker for phux.sh and docs.phux.sh.
// Assets live in ../dist (astro build); ../host/index.ts splits them across the
// two hostnames. The live-demo backend is a separate Worker (../worker).
//
// Deployed by .github/workflows/site-deploy.yml. Paths are relative to this
// directory, where every alchemy command runs.
//
// State: this stack holds a single Worker with a fixed name, and Alchemy
// recognises its own Workers by their `alchemy:*` script tags. So CI starts
// from empty local state on every run and re-adopts the live Worker instead of
// sharing a remote state store (which would need more credentials here).
// `retain()` means no code change can ever make Alchemy delete it.
import * as Alchemy from "alchemy";
import { adopt } from "alchemy/AdoptPolicy";
import * as Cloudflare from "alchemy/Cloudflare";
import { retain } from "alchemy/RemovalPolicy";
import * as State from "alchemy/State";
import { Stack } from "alchemy/Stack";
import * as Effect from "effect/Effect";
import { requirePhuxAccount } from "./account.ts";

export const PRODUCTION_STAGE = "prod";

export default Alchemy.Stack(
  "phux-site",
  { providers: Cloudflare.providers(), state: State.localState() },
  Effect.gen(function* () {
    const { stage } = yield* Stack;
    if (stage !== PRODUCTION_STAGE) {
      return yield* Effect.die(
        new Error(
          `phux-site only deploys stage "${PRODUCTION_STAGE}" (got "${stage}"); every resource is a production singleton.`,
        ),
      );
    }
    yield* requirePhuxAccount;

    const site = yield* Cloudflare.Worker("Site", {
      name: "phux-site",
      main: "../host/index.ts",
      compatibility: { date: "2026-05-01" },
      assets: {
        directory: "../dist",
        // The worker must see every request so host-based redirects fire
        // before the matching HTML asset is served on the wrong hostname.
        runWorkerFirst: true,
        // Bare canonical paths with no trailing slash; /foo resolves foo.html.
        htmlHandling: "auto-trailing-slash",
        notFoundHandling: "none",
      },
      domain: { name: "phux.sh", aliases: ["docs.phux.sh"] },
      workersDev: false,
      observability: { enabled: false },
      env: {
        // TelemetryDO: hourly aggregate counters (ua class, country, agent
        // signals). No IPs, no raw URLs — aggregate-only so /api/telemetry
        // can be public.
        TELEMETRY: Cloudflare.DurableObject("TELEMETRY", {
          className: "TelemetryDO",
        }),
      },
      // Secrets (MEMBER_KEY, MEMBER_CLAIM_KEY, TELEMETRY_INGEST_KEY, ASC_*)
      // are put directly on the Worker (DEPLOY.md) and kept across deploys.
    }).pipe(adopt(true), retain());

    // Private analytics stays off the public network: a same-account service
    // binding to the Worker that no-phux/ops deploys. No shared credential.
    yield* site.bind("Analytics", {
      bindings: [
        { type: "service", name: "ANALYTICS", service: "phux-ops-analytics" },
      ],
    });

    return { worker: site.workerName, urls: site.urls };
  }),
);
