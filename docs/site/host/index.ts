/**
 * phux-site host router.
 *
 * Static assets stay in dist/; this worker only decides which host a path
 * belongs on. See host/routes.ts for the split. It also negotiates markdown
 * for agents (host/markdown.ts), hosts the read-only MCP endpoint
 * (host/mcp.ts), records passive aggregate telemetry (host/telemetry.ts),
 * and serves the public telemetry dashboard API at /api/telemetry.
 */
import { routeRequest } from "./routes";
import { handleAssociationRequest } from "./pairing";
import type { TestFlightEnv } from "./testflight";
import { estimateTokens, htmlToMarkdown, wantsMarkdown } from "./markdown";
import {
  TelemetryDO,
  classifyRequest,
  recordEvents,
  shouldSkipPath,
  type TelemetryNamespace,
  type WaitUntil,
} from "./telemetry";

export { TelemetryDO };

export interface Env extends TestFlightEnv {
  ASSETS: { fetch(input: Request): Promise<Response> };
  TELEMETRY?: TelemetryNamespace;
  /** Shared secret for the demo worker's cross-worker telemetry ingest. */
  TELEMETRY_INGEST_KEY?: string;
  /** Private ops pipeline via same-account Worker service binding. */
  ANALYTICS?: { fetch(input: Request): Promise<Response> };
  MEMBER_KEY?: string;
  MEMBER_CLAIM_KEY?: string;
  MEMBER_CLAIM_KEY_PREVIOUS?: string;
}

export default {
  async fetch(request: Request, env: Env, ctx?: WaitUntil): Promise<Response> {
    const url = new URL(request.url);

    // Apple's association file is answered before any routing, because
    // Universal Links treat a redirect as no association at all. Anything
    // that could 3xx this path silently un-pairs every iOS device, which is
    // precisely the failure this handler exists to prevent (phux-z84.4).
    const association = handleAssociationRequest(url);
    if (association) return association;

    const routed = routeRequest(url.hostname, url);
    if (routed.kind === "redirect") {
      return Response.redirect(routed.location, routed.status);
    }

    // Bundled scripts/styles are host-neutral and carry no visitor telemetry.
    // Keep host routing above this shortcut; do not bypass it for docs assets.
    if (url.pathname.startsWith("/_astro/") && shouldSkipPath(url.pathname)) {
      return env.ASSETS.fetch(request);
    }

    // Hosted MCP endpoint (streamable HTTP, POST /mcp). Load its tool catalogue
    // only on this route, not on every document or asset request.
    if (url.pathname === "/mcp") {
      // Intentional cold-start boundary: a static import initializes the tool
      // catalogue even when this isolate only serves static assets.
      const { handleMcpRequest } = await import("./mcp");
      const mcp = await handleMcpRequest(request, env, (dim, key) =>
        recordEvents(env.TELEMETRY, ctx, [{ dim, key }]),
      );
      if (mcp) {
        recordEvents(
          env.TELEMETRY,
          ctx,
          classifyRequest(request, mcp, [{ dim: "signal", key: "mcp" }]),
        );
        observe(env, ctx, request, mcp);
        return mcp;
      }
    }

    // Public telemetry aggregates (the /telemetry page reads this).
    if (url.pathname === "/api/telemetry") {
      return telemetryApi(request, env);
    }
    // Cross-worker ingest (the demo worker forwards its session events here).
    if (url.pathname === "/api/telemetry/ingest") {
      return telemetryIngest(request, env);
    }
    // Voluntary member signup (the release-updates form), TestFlight access
    // requests for phux-mobile (host/testflight.ts), and device claim.
    if (
      url.pathname === "/api/join" ||
      url.pathname === "/api/beta" ||
      url.pathname === "/api/claim"
    ) {
      // Static imports would initialize the Effect HTTP stack on asset requests.
      const { handleAnalyticsHttp } = await import("./analytics-http");
      return handleAnalyticsHttp(request, env, ctx);
    }

    const asset = await env.ASSETS.fetch(request);
    const contentType = asset.headers.get("content-type") ?? "";
    if (!asset.ok || !contentType.includes("text/html")) {
      recordEvents(env.TELEMETRY, ctx, classifyRequest(request, asset));
      observe(env, ctx, request, asset);
      return asset;
    }

    if (
      (request.method === "GET" || request.method === "HEAD") &&
      wantsMarkdown(request.headers.get("accept"))
    ) {
      const markdown = await markdownResponse(asset);
      recordEvents(env.TELEMETRY, ctx, classifyRequest(request, markdown));
      observe(env, ctx, request, markdown);
      return markdown;
    }

    // HTML responses must declare Vary: Accept so the markdown variant cached
    // for agents is never served to a browser, and vice versa. The Link
    // headers advertise the machine-readable discovery surface (RFC 8288).
    const headers = new Headers(asset.headers);
    appendVary(headers, "Accept");
    headers.set(
      "link",
      '</.well-known/api-catalog>; rel="api-catalog", ' +
        '</llms.txt>; rel="alternate"; type="text/plain", ' +
        '</.well-known/ai-catalog.json>; rel="service-desc"',
    );
    const html = new Response(asset.body, {
      status: asset.status,
      statusText: asset.statusText,
      headers,
    });
    recordEvents(env.TELEMETRY, ctx, classifyRequest(request, html));
    observe(env, ctx, request, html);
    return html;
  },
};

// Forward one exchange to the private ops pipeline. Skips static assets
// (same rule as the public aggregate counters) and never throws.
function observe(
  env: Env,
  ctx: WaitUntil | undefined,
  request: Request,
  response: Response,
): void {
  if (!env.ANALYTICS || shouldSkipPath(new URL(request.url).pathname)) return;
  // Intentional cold-start boundary: static imports initialize Effect and its
  // analytics runtime even for requests that never emit observations.
  // Include initialization in waitUntil so an outstanding event is not dropped.
  const pending = Promise.all([import("effect"), import("./analytics")])
    .then(([{ Effect }, { buildEnvelope, forwardEnvelope, runAnalytics }]) =>
      runAnalytics(
        buildEnvelope(request, response).pipe(
          Effect.flatMap((envelope) => forwardEnvelope(env, envelope)),
          Effect.ignoreCause,
        ),
      ),
    )
    .catch(() => {});
  if (ctx) ctx.waitUntil(pending);
  else void pending;
}

async function markdownResponse(asset: Response): Promise<Response> {
  const markdown = htmlToMarkdown(await asset.text());
  const headers = new Headers({
    "content-type": "text/markdown; charset=utf-8",
    "x-markdown-tokens": String(estimateTokens(markdown)),
  });
  const cacheControl = asset.headers.get("cache-control");
  if (cacheControl) headers.set("cache-control", cacheControl);
  appendVary(headers, "Accept");
  return new Response(markdown, { status: asset.status, headers });
}

async function telemetryApi(request: Request, env: Env): Promise<Response> {
  if (!env.TELEMETRY) {
    return Response.json({ rows: [], generatedAt: new Date().toISOString() });
  }
  const url = new URL(request.url);
  const hours = Math.min(
    168,
    Math.max(
      1,
      Number.parseInt(url.searchParams.get("hours") ?? "48", 10) || 48,
    ),
  );
  const since = new Date(Date.now() - hours * 3_600_000)
    .toISOString()
    .slice(0, 13);
  const upstream = await env.TELEMETRY.getByName("global").fetch(
    new Request(new URL(`/?since=${since}`, url.origin).href),
  );
  const body = await upstream.text();
  return new Response(body, {
    status: upstream.status,
    headers: {
      "content-type": "application/json",
      "cache-control": "no-store",
    },
  });
}

function ingestKeyMatches(
  secret: string | undefined,
  provided: string | null,
): boolean {
  if (!secret) return false;
  const candidate = provided ?? "";
  let difference = candidate.length ^ secret.length;
  const length = Math.max(candidate.length, secret.length);
  for (let index = 0; index < length; index++) {
    difference |=
      (candidate.charCodeAt(index) || 0) ^ (secret.charCodeAt(index) || 0);
  }
  return difference === 0;
}

async function telemetryIngest(request: Request, env: Env): Promise<Response> {
  if (request.method !== "POST")
    return new Response("post only", { status: 405 });
  if (
    !ingestKeyMatches(
      env.TELEMETRY_INGEST_KEY,
      request.headers.get("x-telemetry-key"),
    )
  ) {
    return new Response("unauthorized", { status: 403 });
  }
  let body: { events?: unknown };
  try {
    body = await request.json();
  } catch {
    return new Response("bad json", { status: 400 });
  }
  const events = Array.isArray(body.events) ? body.events : [];
  // Reuse the DO's own ingestion path by forwarding the raw batch.
  const upstream = await env.TELEMETRY!.getByName("global").fetch(
    new Request("https://telemetry.internal/ingest", {
      method: "POST",
      body: JSON.stringify({ events }),
    }),
  );
  return new Response(null, { status: upstream.status });
}

function appendVary(headers: Headers, value: string): void {
  const existing = headers.get("vary");
  if (!existing) {
    headers.set("vary", value);
    return;
  }
  const tokens = existing.split(",").map((token) => token.trim().toLowerCase());
  if (!tokens.includes(value.toLowerCase())) {
    headers.set("vary", `${existing}, ${value}`);
  }
}
