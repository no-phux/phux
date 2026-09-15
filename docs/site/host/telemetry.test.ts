import { describe, expect, test } from "bun:test";
import {
  classifyRequest,
  classifySignal,
  classifyUserAgent,
  shouldSkipPath,
} from "./telemetry";

function htmlResponse(accept?: string) {
  const headers: Record<string, string> = { "content-type": "text/html; charset=utf-8" };
  if (accept) headers["content-type"] = "text/markdown; charset=utf-8";
  return new Response("<html></html>", { status: 200, headers });
}

function requestFor(path: string, init: RequestInit = {}) {
  return new Request(`https://phux.sh${path}`, init);
}

describe("classifyUserAgent", () => {
  test("names the well-known AI crawlers", () => {
    expect(classifyUserAgent("Mozilla/5.0 (compatible; ClaudeBot/1.0)")).toBe("ai:anthropic-bot");
    expect(classifyUserAgent("Mozilla/5.0 (compatible; GPTBot/1.2)")).toBe("ai:openai-bot");
    expect(classifyUserAgent("ChatGPT-User/1.0")).toBe("ai:openai");
    expect(classifyUserAgent("Claude-User")).toBe("ai:anthropic");
    expect(classifyUserAgent("PerplexityBot/1.0")).toBe("ai:perplexity-bot");
    expect(classifyUserAgent("Google-Extended")).toBe("ai:google");
  });

  test("separates search crawlers from browsers and CLI tools", () => {
    expect(classifyUserAgent("Mozilla/5.0 (compatible; Googlebot/2.1)")).toBe("search:google");
    expect(classifyUserAgent("Mozilla/5.0 (Macintosh) AppleWebKit/537.36 Chrome/120")).toBe("browser");
    expect(classifyUserAgent("curl/8.5.0")).toBe("tool:cli-http");
    expect(classifyUserAgent("python-requests/2.31")).toBe("tool:cli-http");
    expect(classifyUserAgent(null)).toBe("other");
    expect(classifyUserAgent("TotallyUnknownThing/9.9")).toBe("other");
  });
});

describe("classifySignal", () => {
  test("marks markdown negotiation", () => {
    const request = new Request("https://phux.sh/", {
      headers: { accept: "text/markdown" },
    });
    const response = new Response("# x", {
      headers: { "content-type": "text/markdown; charset=utf-8" },
    });
    expect(classifySignal(request, response, "/", "other")).toBe("markdown");
  });

  test("marks well-known discovery probes and agent docs", () => {
    const request = requestFor("/");
    const html = htmlResponse();
    expect(classifySignal(request, html, "/.well-known/api-catalog", "other")).toBe("well-known:api-catalog");
    expect(classifySignal(request, html, "/.well-known/agent-skills/index.json", "other")).toBe("well-known:agent-skills");
    expect(classifySignal(request, html, "/llms.txt", "other")).toBe("doc:llms-txt");
    expect(classifySignal(request, html, "/auth.md", "other")).toBe("doc:auth-md");
    expect(classifySignal(request, html, "/install", "other")).toBe("installer");
    expect(classifySignal(request, html, "/", "other")).toBe("page");
    expect(classifySignal(request, html, "/concepts", "other")).toBe("page");
  });

  test("marks MCP calls and scanner robots fetches", () => {
    const mcp = new Request("https://phux.sh/mcp", { method: "POST" });
    expect(classifySignal(mcp, new Response("{}"), "/mcp", "other")).toBe("mcp:call");
    const preflight = new Request("https://phux.sh/mcp", { method: "OPTIONS" });
    expect(classifySignal(preflight, new Response(null, { status: 204 }), "/mcp", "other")).toBe("mcp:preflight");
    const robots = requestFor("/robots.txt");
    expect(classifySignal(robots, new Response("u"), "/robots.txt", "scanner:isitagentready")).toBe("robots:scanner");
    expect(classifySignal(robots, new Response("u"), "/robots.txt", "ai:anthropic-bot")).toBeNull();
  });
});

describe("classifyRequest", () => {
  test("produces dimension events for a page view", () => {
    const request = Object.assign(
      new Request("https://docs.phux.sh/wire/proto", {
        headers: { "user-agent": "ClaudeBot/1.0" },
      }),
      { cf: { country: "DE" } },
    );
    const events = classifyRequest(request, htmlResponse());
    const dims = Object.fromEntries(events.map((e) => [e.dim, e.key]));
    expect(dims.status).toBe("2xx");
    expect(dims.host).toBe("docs.phux.sh");
    expect(dims.ua).toBe("ai:anthropic-bot");
    expect(dims.country).toBe("DE");
    expect(dims.signal).toBe("page");
  });

  test("appends extra events and records 404 misses", () => {
    const request = requestFor("/nope", { headers: { "user-agent": "curl/8" } });
    const events = classifyRequest(request, new Response("nf", { status: 404 }), [
      { dim: "session", key: "demo:edge" },
    ]);
    expect(events).toContainEqual({ dim: "session", key: "demo:edge" });
    expect(events).toContainEqual({ dim: "miss", key: "/nope" });
    expect(events).toContainEqual({ dim: "status", key: "4xx" });
  });
});

describe("shouldSkipPath", () => {
  test("skips static assets and the dashboard API", () => {
    expect(shouldSkipPath("/app.css")).toBe(true);
    expect(shouldSkipPath("/lib/phux_web_bg.wasm")).toBe(true);
    expect(shouldSkipPath("/og.png")).toBe(true);
    expect(shouldSkipPath("/api/telemetry")).toBe(true);
    expect(shouldSkipPath("/api/search.json")).toBe(true);
  });

  test("keeps pages and agent documents", () => {
    expect(shouldSkipPath("/")).toBe(false);
    expect(shouldSkipPath("/wire/proto")).toBe(false);
    expect(shouldSkipPath("/llms.txt")).toBe(false);
    expect(shouldSkipPath("/auth.md")).toBe(false);
    expect(shouldSkipPath("/.well-known/api-catalog")).toBe(false);
    expect(shouldSkipPath("/robots.txt")).toBe(false);
  });
});
