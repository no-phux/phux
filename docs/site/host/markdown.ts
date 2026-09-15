/**
 * Markdown content negotiation for agents.
 *
 * Browsers keep getting HTML; a request whose Accept header advertises
 * `text/markdown` gets a markdown rendering of the same page. Mirrors
 * Cloudflare's Markdown for Agents response contract (Content-Type, Vary:
 * Accept, x-markdown-tokens estimate) so agents can consume
 * phux.sh / docs.phux.sh without scraping page chrome. See
 * https://developers.cloudflare.com/fundamentals/reference/markdown-for-agents/
 *
 * Pure functions so unit tests do not need a Cloudflare runtime.
 */
import { NodeHtmlMarkdown } from "node-html-markdown";

// node-html-markdown reads `process.env.LOG_PERF` (perf logging, off by
// default). The Workers runtime has no `process`; shim it minimally so the
// converter runs without enabling nodejs_compat for the whole worker.
(globalThis as { process?: { env: Record<string, string> } }).process ??= {
  env: {},
};

/** True when the Accept header lists text/markdown with a non-zero q-value. */
export function wantsMarkdown(accept: string | null): boolean {
  if (!accept) return false;
  return accept.split(",").some((entry) => {
    const [mediaType, ...params] = entry.trim().split(";");
    if ((mediaType ?? "").trim().toLowerCase() !== "text/markdown") return false;
    const q = params
      .map((param) => param.trim())
      .find((param) => param.toLowerCase().startsWith("q="));
    return q === undefined || Number.parseFloat(q.slice(2)) > 0;
  });
}

// Page chrome that carries no prose value for an agent. <header> only ever
// holds the nav on this site; the article body lives in <main>.
const DROP_TAGS = [
  "script",
  "style",
  "svg",
  "noscript",
  "iframe",
  "template",
  "form",
  "button",
  "select",
  "nav",
  "header",
  "footer",
];

/** The prose-bearing slice of the document: <main>, else <article>, else <body>. */
export function extractContent(html: string): string {
  const main =
    pickSection(html, "main") ??
    pickSection(html, "article") ??
    pickSection(html, "body") ??
    html;
  let out = main;
  for (const tag of DROP_TAGS) {
    const paired = new RegExp(`<${tag}\\b[^>]*>[\\s\\S]*?</${tag}>`, "gi");
    out = out.replace(paired, "");
    out = out.replace(new RegExp(`<${tag}\\b[^>]*>`, "gi"), "");
  }
  return out;
}

/** YAML frontmatter from the page's meta tags, Cloudflare Markdown-for-Agents shape. */
export function frontmatter(html: string): string {
  const title =
    metaName(html, "title") ??
    metaProperty(html, "og:title") ??
    /<title[^>]*>([^<]*)<\/title>/i.exec(html)?.[1]?.trim() ??
    null;
  const description =
    metaName(html, "description") ?? metaProperty(html, "og:description") ?? null;
  const image = metaProperty(html, "og:image") ?? null;

  const fields: string[] = [];
  if (title) fields.push(`title: ${yamlScalar(title)}`);
  if (description) fields.push(`description: ${yamlScalar(description)}`);
  if (image) fields.push(`image: ${yamlScalar(image)}`);
  return fields.length > 0 ? `---\n${fields.join("\n")}\n---` : "";
}

/** Full document: frontmatter + markdown converted from the content slice. */
export function htmlToMarkdown(html: string): string {
  const matter = frontmatter(html);
  const body = converter.translate(extractContent(html)).trim();
  return matter ? `${matter}\n\n${body}\n` : `${body}\n`;
}

/**
 * Rough token estimate (4 chars/token), the same spirit as Cloudflare's
 * x-markdown-tokens. Cheap on purpose: it is a budgeting hint, not a count.
 */
export function estimateTokens(markdown: string): number {
  return Math.max(1, Math.ceil(markdown.length / 4));
}

const converter = new NodeHtmlMarkdown({
  codeBlockStyle: "fenced",
  bulletMarker: "-",
  emDelimiter: "*",
  maxConsecutiveNewlines: 2,
});

function pickSection(html: string, tag: string): string | null {
  const match = new RegExp(`<${tag}\\b[^>]*>([\\s\\S]*?)</${tag}>`, "i").exec(html);
  return match?.[1] ?? null;
}

function metaName(html: string, name: string): string | null {
  const pattern = new RegExp(
    `<meta\\b[^>]*\\bname=["']${escapeRegExp(name)}["'][^>]*>`,
    "i",
  );
  return metaContent(pattern.exec(html)?.[0] ?? null);
}

function metaProperty(html: string, property: string): string | null {
  const pattern = new RegExp(
    `<meta\\b[^>]*\\bproperty=["']${escapeRegExp(property)}["'][^>]*>`,
    "i",
  );
  return metaContent(pattern.exec(html)?.[0] ?? null);
}

function metaContent(tag: string | null): string | null {
  if (!tag) return null;
  const match = /\bcontent=["']([^"']*)["']/i.exec(tag);
  const value = match?.[1]?.trim();
  return value ? value : null;
}

function yamlScalar(value: string): string {
  const singleLine = value.replace(/[\r\n]+/g, " ").trim();
  return `"${singleLine.replace(/"/g, '\\"')}"`;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
