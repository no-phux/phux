/**
 * phux-site host router.
 *
 * Static assets stay in dist/; this worker only decides which host a path
 * belongs on. See host/routes.ts for the split. It also negotiates markdown
 * for agents: `Accept: text/markdown` gets a markdown rendering of the page
 * while browsers keep getting HTML. See host/markdown.ts.
 */
import { routeRequest } from "./routes";
import { handleMcpRequest } from "./mcp";
import {
  estimateTokens,
  htmlToMarkdown,
  wantsMarkdown,
} from "./markdown";

export interface Env {
  ASSETS: { fetch(input: Request): Promise<Response> };
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const routed = routeRequest(url.hostname, url);
    if (routed.kind === "redirect") {
      return Response.redirect(routed.location, routed.status);
    }

    // Hosted MCP endpoint (streamable HTTP, POST /mcp).
    const mcp = await handleMcpRequest(request, env);
    if (mcp) return mcp;

    const asset = await env.ASSETS.fetch(request);
    const contentType = asset.headers.get("content-type") ?? "";
    if (!asset.ok || !contentType.includes("text/html")) {
      return asset;
    }

    if (
      (request.method === "GET" || request.method === "HEAD") &&
      wantsMarkdown(request.headers.get("accept"))
    ) {
      return markdownResponse(asset);
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
    return new Response(asset.body, {
      status: asset.status,
      statusText: asset.statusText,
      headers,
    });
  },
};

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
