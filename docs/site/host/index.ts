/**
 * phux-site host router.
 *
 * Static assets stay in dist/; this worker only decides which host a path
 * belongs on. See host/routes.ts for the split. It also negotiates markdown
 * for agents (host/markdown.ts), hosts the read-only MCP endpoint
 * (host/mcp.ts), records passive aggregate telemetry (host/telemetry.ts),
 * and serves the public telemetry dashboard API at /api/telemetry.
 */
import { Effect } from "effect";
import { routeRequest } from "./routes";
import { handleMcpRequest } from "./mcp";
import { estimateTokens, htmlToMarkdown, wantsMarkdown } from "./markdown";
import {
  TelemetryDO,
  classifyRequest,
  recordEvents,
  shouldSkipPath,
  type TelemetryNamespace,
  type WaitUntil,
} from "./telemetry";
import {
  buildEnvelope,
  forwardEnvelope,
  runAnalyticsBackground,
} from "./analytics";
import { handleAnalyticsHttp } from "./analytics-http";

export { TelemetryDO };

export interface Env {
  ASSETS: { fetch(input: Request): Promise<Response> };
  TELEMETRY?: TelemetryNamespace;
  /** Shared secret for the demo worker's cross-worker telemetry ingest. */
  TELEMETRY_INGEST_KEY?: string;
  /** Private ops pipeline via same-account Worker service binding. */
  ANALYTICS?: { fetch(input: Request): Promise<Response> };
  /** HTTP fallback for local development and staged migration only. */
  ANALYTICS_INGEST_URL?: string;
  ANALYTICS_INGEST_KEY?: string;
  MEMBER_KEY?: string;
  MEMBER_CLAIM_KEY?: string;
  MEMBER_CLAIM_KEY_PREVIOUS?: string;
}

export default {
  async fetch(
    request: Request,
    env: Env,
    ctx?: WaitUntil,
  ): Promise<Response> {
    const url = new URL(request.url);
    const routed = routeRequest(url.hostname, url);
    if (routed.kind === "redirect") {
      return Response.redirect(routed.location, routed.status);
    }

    // Hosted MCP endpoint (streamable HTTP, POST /mcp).
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

    // Public telemetry aggregates (the /telemetry page reads this).
    if (url.pathname === "/api/telemetry") {
      return telemetryApi(request, env);
    }
    // Cross-worker ingest (the demo worker forwards its session events here).
    if (url.pathname === "/api/telemetry/ingest") {
      return telemetryIngest(request, env);
    }
    // Voluntary member signup (the join-the-beta form) and device claim.
    if (url.pathname === "/api/join" || url.pathname === "/api/claim") {
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
  if (shouldSkipPath(new URL(request.url).pathname)) return;
  runAnalyticsBackground(
    ctx,
    buildEnvelope(request, response).pipe(
      Effect.flatMap((envelope) => forwardEnvelope(env, envelope)),
    ),
  );
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
  const hours = Math.min(168, Math.max(1, Number.parseInt(url.searchParams.get("hours") ?? "48", 10) || 48));
  const since = new Date(Date.now() - hours * 3_600_000).toISOString().slice(0, 13);
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

async function telemetryIngest(request: Request, env: Env): Promise<Response> {
  if (request.method !== "POST") return new Response("post only", { status: 405 });
  if (!env.TELEMETRY_INGEST_KEY || request.headers.get("x-telemetry-key") !== env.TELEMETRY_INGEST_KEY) {
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
