import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

import { boundedResult } from "@phux/integration-runtime/tools";

const EXA_MCP_URL = "https://mcp.exa.ai/mcp";
const EXA_API_URL = "https://api.exa.ai/search";
const TIMEOUT_MS = 30_000;
const MAX_FETCH_BYTES = 2 * 1024 * 1024;

export type Fetch = typeof fetch;

/** An Exa key from `EXA_API_KEY` or pi-web-access's `web-search.json`; keyless Exa MCP otherwise. */
export function exaKey(env: NodeJS.ProcessEnv = process.env): string | undefined {
  if (env.EXA_API_KEY?.trim()) return env.EXA_API_KEY.trim();
  const dir = env.PI_CODING_AGENT_DIR?.trim() || join(homedir(), ".pi", "agent");
  try {
    const config = JSON.parse(readFileSync(join(dir, "web-search.json"), "utf8")) as { exaApiKey?: unknown };
    return typeof config.exaApiKey === "string" && config.exaApiKey.trim() ? config.exaApiKey.trim() : undefined;
  } catch {
    return undefined;
  }
}

export async function webSearch(query: string, count: number, options: { key?: string | undefined; fetch?: Fetch; signal?: AbortSignal } = {}): Promise<string> {
  const doFetch = options.fetch ?? fetch;
  const signal = withTimeout(options.signal);
  if (options.key !== undefined) {
    const response = await doFetch(EXA_API_URL, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": options.key },
      body: JSON.stringify({ query, numResults: count, contents: { highlights: { numSentences: 3 } } }),
      signal,
    });
    if (!response.ok) throw new Error(`Exa search failed: HTTP ${response.status} ${(await response.text()).slice(0, 200)}`);
    const body = (await response.json()) as { results?: { title?: string; url?: string; publishedDate?: string; highlights?: string[] }[] };
    const results = body.results ?? [];
    if (results.length === 0) return "No results.";
    return results.map((result) => [
      `Title: ${result.title ?? ""}`,
      `URL: ${result.url ?? ""}`,
      ...(result.publishedDate ? [`Published: ${result.publishedDate}`] : []),
      ...(result.highlights ?? []).map((line) => `> ${line}`),
    ].join("\n")).join("\n\n");
  }
  const response = await doFetch(`${EXA_MCP_URL}?tools=web_search_exa`, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json, text/event-stream" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "web_search_exa", arguments: { query, numResults: count } } }),
    signal,
  });
  if (!response.ok) {
    const hint = response.status === 429 ? " (keyless Exa is rate limited; set EXA_API_KEY)" : "";
    throw new Error(`Exa search failed: HTTP ${response.status}${hint}`);
  }
  return mcpText(await response.text());
}

/** The first text result of a JSON-RPC reply, plain or as server-sent events. */
export function mcpText(body: string): string {
  const candidates = body.split("\n").filter((line) => line.startsWith("data:")).map((line) => line.slice(5).trim());
  candidates.push(body);
  for (const candidate of candidates) {
    let parsed: { result?: { isError?: boolean; content?: { type?: string; text?: string }[] }; error?: { message?: string } };
    try {
      parsed = JSON.parse(candidate);
    } catch {
      continue;
    }
    if (parsed.error) throw new Error(`Exa search failed: ${parsed.error.message ?? "unknown error"}`);
    const text = parsed.result?.content?.find((item) => item.type === "text" && item.text?.trim())?.text;
    if (parsed.result?.isError) throw new Error(text ?? "Exa search failed");
    if (text) return text;
  }
  throw new Error("Exa search returned no results");
}

export async function webFetch(url: string, options: { fetch?: Fetch; signal?: AbortSignal } = {}): Promise<string> {
  const parsed = new URL(url);
  if (parsed.protocol !== "https:" && parsed.protocol !== "http:") throw new Error("web_fetch reads http(s) URLs only");
  const response = await (options.fetch ?? fetch)(parsed, {
    headers: { "user-agent": "phux-chief/0.1", accept: "text/html,text/plain,application/json;q=0.9,*/*;q=0.5" },
    redirect: "follow",
    signal: withTimeout(options.signal),
  });
  const type = response.headers.get("content-type") ?? "";
  const raw = (await readBounded(response, MAX_FETCH_BYTES)).trim();
  const text = type.includes("html") ? htmlToText(raw) : raw;
  return boundedResult(`GET ${response.url || parsed.href} → ${response.status} ${type}`, text).text;
}

async function readBounded(response: Response, max: number): Promise<string> {
  if (response.body === null) return "";
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (total < max) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    total += value.byteLength;
  }
  await reader.cancel().catch(() => {});
  return Buffer.concat(chunks).subarray(0, max).toString("utf8");
}

/** Readable text from HTML: scripts, styles and tags dropped, block breaks kept. */
export function htmlToText(html: string): string {
  return html
    .replace(/<(script|style|noscript|svg|head)\b[\s\S]*?<\/\1>/gi, "")
    .replace(/<\/(p|div|section|article|h[1-6]|li|tr|pre|blockquote)>|<br\s*\/?>/gi, "\n")
    .replace(/<li\b[^>]*>/gi, "- ")
    .replace(/<[^>]+>/g, "")
    .replace(/&nbsp;/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, "\"")
    .replace(/&#39;/g, "'")
    .replace(/[ \t]+/g, " ")
    .replace(/\n\s*\n\s*/g, "\n\n")
    .trim();
}

function withTimeout(signal: AbortSignal | undefined): AbortSignal {
  const timeout = AbortSignal.timeout(TIMEOUT_MS);
  return signal === undefined ? timeout : AbortSignal.any([signal, timeout]);
}
