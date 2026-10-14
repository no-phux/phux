/**
 * Hosted MCP endpoint for phux.sh (streamable HTTP, POST-only).
 *
 * The site already serves everything an agent needs — a docs search index,
 * every page as markdown (Accept negotiation), install commands, release
 * metadata — so the MCP server is a thin, read-only JSON-RPC adapter over
 * those static facts. No state, no credentials, no user data.
 *
 * Implements the MCP streamable HTTP transport subset a static site can
 * honestly support: POST /mcp with JSON-RPC 2.0 (initialize, ping,
 * tools/list, tools/call). Server-Sent Events (GET) is not implemented;
 * clients fall back to POST.
 *
 * See host/markdown.ts for the markdown conversion and
 * .well-known/mcp/server-card.json for the published card.
 */
import { htmlToMarkdown } from "./markdown";
import { isDocsPath, routeRequest } from "./routes";

// MCP spec versions this server implements, oldest first. Clients negotiate in
// initialize: a requested version from this list is echoed back, anything else
// falls back to DEFAULT. Every version here must be a real, released spec
// date — clients hard-reject unknown versions (a made-up date like
// "2025-06-15" breaks every connecting client).
const SUPPORTED_PROTOCOL_VERSIONS = ["2024-11-05", "2025-03-26", "2025-06-18"] as const;
const DEFAULT_PROTOCOL_VERSION = "2025-03-26";
const PROTOCOL_VERSION = DEFAULT_PROTOCOL_VERSION;
const SERVER_NAME = "phux-site";
const SERVER_VERSION = "1.0.0";

function isSupportedProtocolVersion(value: unknown): value is string {
  return (
    typeof value === "string" &&
    (SUPPORTED_PROTOCOL_VERSIONS as readonly string[]).includes(value)
  );
}

export interface McpEnv {
  ASSETS: { fetch(input: Request): Promise<Response> };
}

interface ToolDef {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
  run(params: Record<string, unknown>, env: McpEnv): Promise<string>;
}

const INSTALL_COMMANDS = {
  cli: "curl -fsSL https://phux.sh/install | sh",
  cockpit: "curl -fsSL https://phux.sh/install-cockpit | sh",
  desktop: "curl -fsSL https://phux.sh/install-desktop | sh",
  skills: "npx skills add no-phux/skills",
} as const;

const TOOLS: ToolDef[] = [
  {
    name: "search_docs",
    description:
      "Search the phux documentation (docs.phux.sh). Returns the top matching pages with title, URL, and a snippet. Use this to find the right guide before fetching a page.",
    inputSchema: {
      type: "object",
      properties: {
        query: { type: "string", description: "Search terms, e.g. \"attach to a remote terminal\"" },
      },
      required: ["query"],
    },
    run: async (params, env) => {
      const query = String(params.query ?? "").trim();
      if (!query) return "query is required";
      const index = await loadIndex(env);
      const terms = query.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
      const scored = index.pages
        .map((page) => ({ page, score: scorePage(page, terms) }))
        .filter((entry) => entry.score > 0)
        .sort((a, b) => b.score - a.score)
        .slice(0, 5);
      if (scored.length === 0) return `No docs matched "${query}". Try fewer or broader terms.`;
      return scored
        .map(({ page }) => `## ${page.title}\n${page.url}\n${page.description ?? ""}`)
        .join("\n\n");
    },
  },
  {
    name: "get_page",
    description:
      "Fetch any phux.sh or docs.phux.sh page as clean markdown (no HTML chrome). Accepts a path like \"/quickstart/install\" or a full URL on those hosts.",
    inputSchema: {
      type: "object",
      properties: {
        path: { type: "string", description: "Page path or URL, e.g. \"/wire/proto\"" },
      },
      required: ["path"],
    },
    run: async (params, env) => {
      const raw = String(params.path ?? "").trim();
      const url = resolvePageUrl(raw);
      if (!url) return "Only https://phux.sh and https://docs.phux.sh pages can be fetched.";
      const asset = await fetchPageAsset(url, env);
      if (!asset.ok) return `Page not found: ${url.pathname}`;
      const type = asset.headers.get("content-type") ?? "";
      if (type.includes("text/html")) {
        return htmlToMarkdown(await asset.text());
      }
      if (type.startsWith("text/") || type.includes("json") || url.pathname.endsWith(".md")) {
        return await asset.text();
      }
      return `${url.pathname} is not a textual page (content-type: ${type}).`;
    },
  },
  {
    name: "get_install_command",
    description:
      "Get the one-line installer for the phux CLI, GPUI desktop, Cockpit, or agent skills.",
    inputSchema: {
      type: "object",
      properties: {
        target: {
          type: "string",
          enum: ["cli", "desktop", "cockpit", "skills"],
          description: "What to install. Defaults to the phux CLI.",
        },
      },
    },
    run: async (params) => {
      const target = params.target;
      if (target === "desktop" || target === "cockpit" || target === "skills") {
        return INSTALL_COMMANDS[target];
      }
      return INSTALL_COMMANDS.cli;
    },
  },
  {
    name: "latest_release",
    description:
      "Get the latest phux and Cockpit release tags, URLs, and publish dates.",
    inputSchema: { type: "object", properties: {} },
    run: async (_params, env) => {
      const index = await loadIndex(env);
      const { phux, cockpit } = index.release;
      return [
        `phux ${phux.tag} — ${phux.url} (published ${phux.publishedAt})`,
        `Cockpit ${cockpit.tag} — ${cockpit.url} (published ${cockpit.publishedAt})`,
      ].join("\n");
    },
  },
];

/** Resolve public host redirects without making arbitrary network requests. */
function resolvePageUrl(raw: string): URL | null {
  let url: URL;
  try {
    url = new URL(raw, "https://phux.sh");
  } catch {
    return null;
  }
  if (!["https://phux.sh", "https://docs.phux.sh"].includes(url.origin)) return null;
  if (url.username || url.password) return null;
  // Never re-parse a scheme-relative pathname as another host.
  if (url.pathname.startsWith("//")) return null;
  const route = routeRequest(url.hostname, url);
  return route.kind === "redirect" ? new URL(route.location) : url;
}

async function fetchPageAsset(url: URL, env: McpEnv): Promise<Response> {
  const pathname = url.pathname.replace(/\/$/, "").replace(/\.html$/, "");
  if (isDocsPath(pathname) && !/\.[^/]+$/.test(pathname)) {
    const markdownUrl = new URL(`${pathname}.md`, url.origin);
    const markdown = await env.ASSETS.fetch(new Request(markdownUrl));
    if (markdown.ok) return markdown;
    // Old redirect-only URLs have no collection entry; retain the HTML fallback.
  }
  return env.ASSETS.fetch(new Request(url));
}

let indexCache: { pages: SearchPage[]; release: ReleaseInfo } | null = null;

interface SearchPage {
  title: string;
  url: string;
  description?: string;
  text: string;
}

interface ReleaseInfo {
  phux: { tag: string; url: string; publishedAt: string };
  cockpit: { tag: string; url: string; publishedAt: string };
}

async function loadIndex(env: McpEnv): Promise<{ pages: SearchPage[]; release: ReleaseInfo }> {
  if (indexCache) return indexCache;
  const asset = await env.ASSETS.fetch(
    new Request("https://phux.sh/api/mcp-search.json"),
  );
  if (!asset.ok) {
    return { pages: [], release: placeholderRelease() };
  }
  const parsed = (await asset.json()) as {
    pages: SearchPage[];
    release: ReleaseInfo;
  };
  indexCache = parsed;
  return parsed;
}

function placeholderRelease(): ReleaseInfo {
  return {
    phux: { tag: "unknown", url: "https://github.com/no-phux/phux/releases", publishedAt: "unknown" },
    cockpit: {
      tag: "unknown",
      url: "https://github.com/no-phux/phux/releases",
      publishedAt: "unknown",
    },
  };
}

function scorePage(page: SearchPage, terms: string[]): number {
  const haystacks = [
    { text: page.title.toLowerCase(), weight: 3 },
    { text: (page.description ?? "").toLowerCase(), weight: 2 },
    { text: page.text.toLowerCase(), weight: 1 },
  ];
  let score = 0;
  for (const term of terms) {
    for (const { text, weight } of haystacks) {
      let at = 0;
      let count = 0;
      while ((at = text.indexOf(term, at)) !== -1) {
        count++;
        at += term.length;
      }
      score += weight * Math.min(count, 3);
    }
  }
  return score;
}

/** Handle a request aimed at the MCP endpoint; null means "not mine, keep routing". */
export async function handleMcpRequest(
  request: Request,
  env: McpEnv,
  observe?: (dim: string, key: string) => void,
): Promise<Response | null> {
  const url = new URL(request.url);
  if (url.pathname !== "/mcp") return null;

  if (request.method === "OPTIONS") {
    return new Response(null, { status: 204, headers: corsHeaders() });
  }
  if (request.method !== "POST") {
    return jsonRpcError(null, -32600, "POST only; this server does not stream SSE", 405);
  }

  let message: { id?: unknown; method?: string; params?: Record<string, unknown> };
  try {
    message = await request.json();
  } catch {
    return jsonRpcError(null, -32700, "parse error", 400);
  }

  const { id, method, params = {} } = message;
  // Notifications have no id and never get a response body.
  if (id === undefined || id === null) {
    return new Response(null, { status: 202, headers: corsHeaders(headerVersion(request)) });
  }

  switch (method) {
    case "initialize": {
      observe?.("signal", "mcp:initialize");
      // Negotiate per spec: echo a supported requested version, otherwise
      // fall back to the default so old and new clients both connect.
      const negotiated = isSupportedProtocolVersion(params.protocolVersion)
        ? params.protocolVersion
        : headerVersion(request);
      return jsonRpcResult(
        id,
        {
          protocolVersion: negotiated,
          capabilities: { tools: { listChanged: false } },
          serverInfo: { name: SERVER_NAME, version: SERVER_VERSION },
        },
        negotiated,
      );
    }
    case "ping":
      return jsonRpcResult(id, {});
    case "tools/list":
      return jsonRpcResult(id, {
        tools: TOOLS.map(({ name, description, inputSchema }) => ({
          name,
          description,
          inputSchema,
        })),
      });
    case "tools/call": {
      const name = String(params.name ?? "");
      observe?.("signal", `mcp:tool:${name.slice(0, 40)}`);
      const tool = TOOLS.find((candidate) => candidate.name === name);
      if (!tool) {
        return jsonRpcResult(id, {
          content: [{ type: "text", text: `unknown tool: ${name}` }],
          isError: true,
        });
      }
      try {
        const text = await tool.run(
          (params.arguments as Record<string, unknown>) ?? {},
          env,
        );
        return jsonRpcResult(id, { content: [{ type: "text", text }] });
      } catch (err) {
        return jsonRpcResult(id, {
          content: [{ type: "text", text: `tool error: ${String(err)}` }],
          isError: true,
        });
      }
    }
    default:
      return jsonRpcError(id, -32601, `method not found: ${String(method)}`, 200);
  }
}

function headerVersion(request: Request): string {
  const requested = request.headers.get("mcp-protocol-version");
  return isSupportedProtocolVersion(requested) ? requested : DEFAULT_PROTOCOL_VERSION;
}

function corsHeaders(protocolVersion: string = DEFAULT_PROTOCOL_VERSION): Headers {
  return new Headers({
    "access-control-allow-origin": "*",
    "access-control-allow-methods": "POST, OPTIONS",
    "access-control-allow-headers": "content-type, mcp-protocol-version",
    "access-control-expose-headers": "mcp-protocol-version",
    "mcp-protocol-version": protocolVersion,
  });
}

function jsonRpcResult(id: unknown, result: unknown, protocolVersion?: string): Response {
  return jsonResponse({ jsonrpc: "2.0", id, result }, 200, protocolVersion);
}

function jsonRpcError(id: unknown, code: number, message: string, status: number): Response {
  return jsonResponse({ jsonrpc: "2.0", id, error: { code, message } }, status);
}

function jsonResponse(body: unknown, status = 200, protocolVersion?: string): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { ...objectFromHeaders(corsHeaders(protocolVersion)), "content-type": "application/json" },
  });
}

function objectFromHeaders(headers: Headers): Record<string, string> {
  const out: Record<string, string> = {};
  headers.forEach((value, key) => {
    out[key] = value;
  });
  return out;
}
