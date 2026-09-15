/**
 * Passive edge telemetry for phux.sh and the demo worker.
 *
 * The Workers already see every request — including AI crawlers and tools
 * that never run JavaScript, which is exactly the traffic client-side
 * analytics is blind to. This module turns that visibility into
 * aggregate-only counters: one Durable Object holds hourly buckets keyed by
 * dimension (user-agent class, country, host, agent signal, status, demo
 * session). No IPs, no raw URLs, no cookies — nothing that could identify a
 * visitor, so the dashboard can be public.
 *
 * Recording is fire-and-forget via `ctx.waitUntil` and swallows all errors:
 * telemetry must never slow or break a real request.
 *
 * Pure classification functions live at the bottom and are unit-tested
 * without a Cloudflare runtime.
 */

// ── Durable Object ──────────────────────────────────────────────────────────

interface Sql {
  exec(query: string, ...bindings: (string | number)[]): unknown;
}

interface SqlStorage {
  storage: { sql: Sql };
}

const RETENTION_DAYS = 30;

export class TelemetryDO {
  constructor(state: SqlStorage, _env: unknown) {
    state.storage.sql.exec(`CREATE TABLE IF NOT EXISTS counters (
      bucket TEXT NOT NULL,
      dim TEXT NOT NULL,
      key TEXT NOT NULL,
      count INTEGER NOT NULL DEFAULT 0,
      PRIMARY KEY (bucket, dim, key)
    )`);
    this.sql = state.storage.sql;
  }

  private readonly sql: Sql;

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);

    if (request.method === "POST") {
      let body: { events?: TelemetryEvent[] };
      try {
        body = await request.json();
      } catch {
        return new Response("bad json", { status: 400 });
      }
      const events = Array.isArray(body.events) ? body.events : [];
      for (const event of events.slice(0, 64)) {
        if (!event || typeof event.dim !== "string" || typeof event.key !== "string") {
          continue;
        }
        const dim = event.dim.slice(0, 24);
        const key = event.key.slice(0, 80);
        const count = Number.isFinite(event.count) && event.count! > 0 ? Math.floor(event.count!) : 1;
        this.sql.exec(
          `INSERT INTO counters (bucket, dim, key, count) VALUES (?, ?, ?, ?)
           ON CONFLICT (bucket, dim, key) DO UPDATE SET count = count + excluded.count`,
          currentBucket(),
          dim,
          key,
          count,
        );
      }
      // Cheap retention: amortized across writes.
      if (Math.random() < 0.01) {
        this.sql.exec(
          `DELETE FROM counters WHERE bucket < ?`,
          new Date(Date.now() - RETENTION_DAYS * 86_400_000).toISOString().slice(0, 13),
        );
      }
      return new Response(null, { status: 204 });
    }

    const since = url.searchParams.get("since") ?? defaultSince();
    const rows: CounterRow[] = [];
    const cursor = this.sql.exec(
      `SELECT bucket, dim, key, count FROM counters WHERE bucket >= ? ORDER BY bucket`,
      since.slice(0, 13),
    ) as unknown as { toArray(): CounterRow[] };
    for (const row of cursor.toArray()) rows.push(row);
    return Response.json({ generatedAt: new Date().toISOString(), rows });
  }
}

interface CounterRow {
  bucket: string;
  dim: string;
  key: string;
  count: number;
}

export interface TelemetryEvent {
  dim: string;
  key: string;
  count?: number;
}

export interface TelemetryNamespace {
  getByName(name: string): { fetch(input: Request): Promise<Response> };
}

export interface WaitUntil {
  waitUntil(promise: Promise<unknown>): void;
}

// ── recording ───────────────────────────────────────────────────────────────

const DO_NAME = "global";

/**
 * Buffer events and flush them. Every call flushes whatever is buffered, so
 * bursts amortize into one write each. `namespace` writes to this worker's
 * own TelemetryDO; `forwardTo` POSTs to another worker's ingest route (used
 * by the demo worker, whose Durable Objects live in a separate worker).
 */
export function recordEvents(
  namespace: TelemetryNamespace | undefined,
  ctx: WaitUntil | undefined,
  events: TelemetryEvent[],
): void {
  if (!namespace || events.length === 0) return;
  enqueue(ctx, { target: "local", events, namespace });
}

export function forwardEvents(
  url: string | undefined,
  key: string | undefined,
  ctx: WaitUntil | undefined,
  events: TelemetryEvent[],
): void {
  if (!url || !key || events.length === 0) return;
  enqueue(ctx, { target: "remote", events, url, key });
}

interface LocalBatch {
  target: "local";
  events: TelemetryEvent[];
  namespace: TelemetryNamespace;
}

interface RemoteBatch {
  target: "remote";
  events: TelemetryEvent[];
  url: string;
  key: string;
}

const pendingBatches: Array<LocalBatch | RemoteBatch> = [];
let pendingChain: Promise<unknown> | null = null;

function enqueue(ctx: WaitUntil | undefined, batch: LocalBatch | RemoteBatch): void {
  pendingBatches.push(batch);
  const chain = pendingChain ?? Promise.resolve();
  pendingChain = chain.then(flushBatches).catch(() => {
    // Telemetry is best-effort; never surface failures to a request.
  });
  ctx?.waitUntil(pendingChain);
}

async function flushBatches(): Promise<void> {
  const batches = pendingBatches.splice(0, pendingBatches.length);
  for (const batch of batches) {
    if (batch.target === "local") {
      await batch.namespace
        .getByName(DO_NAME)
        .fetch(
          new Request("https://telemetry.internal/ingest", {
            method: "POST",
            body: JSON.stringify({ events: batch.events }),
          }),
        );
    } else {
      await fetch(batch.url, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-telemetry-key": batch.key,
        },
        body: JSON.stringify({ events: batch.events }),
      });
    }
  }
}

/** Classify one HTTP exchange into a handful of dimension events. */
export function classifyRequest(
  request: Request,
  response: Response,
  extra: TelemetryEvent[] = [],
): TelemetryEvent[] {
  const url = new URL(request.url);
  const pathname = url.pathname;
  const events: TelemetryEvent[] = [...extra];

  if (shouldSkipPath(pathname)) return events;

  const statusClass = `${Math.floor(response.status / 100)}xx`;
  events.push({ dim: "status", key: statusClass });

  const host = url.hostname;
  events.push({
    dim: "host",
    key:
      host === "phux.sh" || host === "www.phux.sh"
        ? "phux.sh"
        : host === "docs.phux.sh"
          ? "docs.phux.sh"
          : host === "shell.phux.sh"
            ? "shell.phux.sh"
            : "preview/other",
  });

  const ua = classifyUserAgent(request.headers.get("user-agent"));
  events.push({ dim: "ua", key: ua });

  const country = (request as Request & { cf?: { country?: string } }).cf?.country;
  if (country) events.push({ dim: "country", key: country });

  const signal = classifySignal(request, response, pathname, ua);
  if (signal) events.push({ dim: "signal", key: signal });

  if (response.status === 404) {
    events.push({ dim: "miss", key: pathname.slice(0, 80) });
  }
  return events;
}

function currentBucket(): string {
  return new Date().toISOString().slice(0, 13);
}

function defaultSince(): string {
  return new Date(Date.now() - 48 * 3_600_000).toISOString().slice(0, 13);
}

// ── classification (pure) ───────────────────────────────────────────────────

const USER_AGENT_CLASSES: Array<[RegExp, string]> = [
  [/isitagentready/i, "scanner:isitagentready"],
  [/ClaudeBot/i, "ai:anthropic-bot"],
  [/Claude-User|Claude-Web|anthropic/i, "ai:anthropic"],
  [/GPTBot/i, "ai:openai-bot"],
  [/ChatGPT-User|OAI-SearchBot/i, "ai:openai"],
  [/Google-Extended/i, "ai:google"],
  [/PerplexityBot/i, "ai:perplexity-bot"],
  [/Perplexity-User/i, "ai:perplexity"],
  [/Bytespider/i, "ai:bytedance"],
  [/Amazonbot/i, "ai:amazon"],
  [/Applebot/i, "ai:apple"],
  [/Cohere/i, "ai:cohere"],
  [/Googlebot/i, "search:google"],
  [/bingbot/i, "search:bing"],
  [/DuckDuckBot/i, "search:ddg"],
  [/^curl\/|^Wget\/|python-requests|aiohttp|httpx|Go-http-client|^node\b|undici|PostmanRuntime|HTTPie/i, "tool:cli-http"],
  [/Mozilla\/5\.0|AppleWebKit/i, "browser"],
  [/.*/, "other"],
];

export function classifyUserAgent(ua: string | null): string {
  if (!ua) return "other";
  for (const [pattern, label] of USER_AGENT_CLASSES) {
    if (pattern.test(ua)) return label;
  }
  return "other";
}

const WELL_KNOWN_SIGNALS: Array<[RegExp, string]> = [
  [/^\/\.well-known\/api-catalog$/, "well-known:api-catalog"],
  [/^\/\.well-known\/ai-catalog\.json$/, "well-known:ai-catalog"],
  [/^\/\.well-known\/mcp\//, "well-known:mcp-card"],
  [/^\/\.well-known\/agent-skills\//, "well-known:agent-skills"],
  [/^\/\.well-known\/agent-card\.json$/, "well-known:a2a-card"],
  [/^\/\.well-known\/openid-configuration$/, "well-known:oidc"],
  [/^\/\.well-known\/oauth-/, "well-known:oauth"],
];

export function classifySignal(
  request: Request,
  response: Response,
  pathname: string,
  uaClass: string,
): string | null {
  if (pathname === "/mcp") {
    return request.method === "OPTIONS" ? "mcp:preflight" : "mcp:call";
  }
  if (
    response.headers.get("content-type")?.includes("text/markdown") &&
    request.headers.get("accept")?.includes("text/markdown")
  ) {
    return "markdown";
  }
  for (const [pattern, label] of WELL_KNOWN_SIGNALS) {
    if (pattern.test(pathname)) return label;
  }
  if (pathname === "/llms.txt") return "doc:llms-txt";
  if (pathname === "/auth.md") return "doc:auth-md";
  if (pathname === "/robots.txt") return uaClass.startsWith("scanner:") ? "robots:scanner" : null;
  if (pathname.startsWith("/install")) return "installer";
  if ((response.headers.get("content-type") ?? "").includes("text/html")) {
    return request.method === "GET" ? "page" : null;
  }
  return null;
}

/** Static assets carry no visitor signal; skip them entirely. */
export function shouldSkipPath(pathname: string): boolean {
  if (pathname === "/api/telemetry") return true; // dashboard self-refresh
  return /\.(css|js|mjs|map|png|jpe?g|gif|svgz?|ico|wasm|xml|json|woff2?|ttf|otf|sh)$/i.test(pathname);
}
