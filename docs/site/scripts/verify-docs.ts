#!/usr/bin/env bun
/** Agent-facing docs acceptance against built assets, never a user's phux server. */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, readFile } from "node:fs/promises";
import { extname, resolve } from "node:path";
import { chromium, webkit, type Page } from "playwright-core";
import handler from "../host/index";
import { htmlToMarkdown } from "../host/markdown";
import { AGENT_START } from "../src/lib/site";

const root = resolve(import.meta.dir, "../dist");
const useWebKit = process.argv.includes("--webkit");
const artifacts = resolve(import.meta.dir, "../.docs-check", useWebKit ? "webkit" : "chrome");
const types: Record<string, string> = {
  ".html": "text/html", ".md": "text/markdown; charset=utf-8", ".txt": "text/plain",
  ".json": "application/json", ".js": "text/javascript", ".css": "text/css",
  ".svg": "image/svg+xml", ".woff2": "font/woff2", ".png": "image/png",
};
async function asset(request: Request) {
  const pathname = new URL(request.url).pathname;
  let path = resolve(root, `.${pathname}`);
  if (!path.startsWith(`${root}/`)) return new Response(null, { status: 403 });
  if (!extname(path)) path += ".html";
  const file = Bun.file(path);
  if (!(await file.exists())) return new Response(null, { status: 404 });
  return new Response(request.method === "HEAD" ? null : file, {
    headers: { "content-type": types[extname(path)] ?? file.type },
  });
}

async function verifyPublishedContent() {
  const index = JSON.parse(await readFile(resolve(root, "api/mcp-search.json"), "utf8"));
  for (const { url } of index.pages as { url: string }[]) {
    const markdown = await readFile(resolve(root, `.${url}.md`), "utf8");
    assert.match(markdown, /^---\n/);
    assert.match(markdown, /\n# .+/);
    assert.match(markdown, /source: "https:\/\/github.com\/no-phux\/phux\/blob\/[0-9a-f]{40}\//);
    assert.ok(!/^\w+: undefined$/m.test(markdown) && !markdown.endsWith("\n\nundefined"), `${url}: complete Markdown`);
  }
  const overview = await readFile(resolve(root, "overview.md"), "utf8");
  assert.ok(overview.includes(AGENT_START.prompt));
  assert.ok(overview.includes("[All documentation](/docs)"));
  assert.ok(overview.includes("cannot see or control your terminals"));
  const negotiated = htmlToMarkdown(await readFile(resolve(root, "overview.html"), "utf8"));
  assert.ok(negotiated.includes(AGENT_START.prompt), "negotiated Markdown retains the handoff");
  assert.ok(negotiated.includes("[All documentation](/docs)"), "negotiated Markdown retains reference links");

  const skills = JSON.parse(await readFile(resolve(root, ".well-known/agent-skills/index.json"), "utf8"));
  assert.deepEqual(skills.skills.map((skill: { name: string }) => skill.name), ["using-phux", "using-phux-mcp", "using-phux-tools"]);
  for (const skill of skills.skills) {
    const body = await readFile(resolve(root, `.${skill.url}`));
    assert.equal(`sha256:${createHash("sha256").update(body).digest("hex")}`, skill.digest);
  }
  const catalog = JSON.parse(await readFile(resolve(root, ".well-known/ai-catalog.json"), "utf8"));
  const llms = await readFile(resolve(root, "llms.txt"), "utf8");
  for (const skill of skills.skills) {
    assert.ok(catalog.entries.some((entry: { url: string }) => entry.url.endsWith(skill.url)));
    assert.ok(llms.includes(skill.name));
  }
  for (const match of llms.matchAll(/\]\((https:\/\/(?:docs\.)?phux\.sh[^)]+)\)/g)) {
    const path = new URL(match[1]!).pathname;
    if (path === "/mcp") continue; // handled by the host, not a static page
    const response = await asset(new Request(`http://localhost${path}`));
    assert.ok(response.ok, `llms.txt link resolves: ${path}`);
  }
  return index.pages.length;
}

async function verifyPageCopy(page: Page, base: string, path: string) {
  await page.goto(`${base}${path}`);
  const actions = page.locator("[data-page-actions]");
  assert.equal(await actions.getByRole("link", { name: "Open Markdown" }).getAttribute("href"), `${path}.md`);
  await actions.getByRole("button", { name: "Copy page for agent" }).click();
  await actions.getByRole("status").filter({ hasText: "Page copied as Markdown." }).waitFor();
  assert.equal(await readClipboard(page), await readFile(resolve(root, `.${path}.md`), "utf8"));
}

async function readClipboard(page: Page): Promise<string> {
  if (!useWebKit) return page.evaluate(() => navigator.clipboard.readText());
  // WebKit cannot pre-grant clipboard permissions. Native paste also proves
  // the async write worked without asking a script to read the clipboard.
  await page.evaluate(() => {
    const input = document.createElement("textarea");
    input.id = "clipboard-check";
    document.body.append(input);
    input.focus();
  });
  await page.keyboard.press(process.platform === "darwin" ? "Meta+V" : "Control+V");
  const text = await page.locator("#clipboard-check").inputValue();
  await page.locator("#clipboard-check").evaluate((input) => input.remove());
  return text;
}

const count = await verifyPublishedContent();
const server = Bun.serve({
  hostname: "127.0.0.1", port: 0,
  fetch: (request) => handler.fetch(request, { ASSETS: { fetch: asset } }),
});
const base = `http://127.0.0.1:${server.port}`;
const browser = useWebKit
  ? await webkit.launch({ headless: true })
  : await chromium.launch({ channel: "chrome", headless: true });
try {
  await mkdir(artifacts, { recursive: true });
  const context = await browser.newContext(useWebKit ? {} : { permissions: ["clipboard-read", "clipboard-write"] });
  const page = await context.newPage();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await verifyPageCopy(page, base, "/overview");
  await page.getByRole("button", { name: "Copy agent prompt" }).click();
  await page.locator(".agent-prompt [role=status]").filter({ hasText: "Agent prompt copied." }).waitFor();
  assert.equal(await readClipboard(page), AGENT_START.prompt);
  assert.ok(await page.getByRole("link", { name: "Choose your agent integration" }).isVisible());
  for (const width of [320, 390, 768, 1280]) {
    await page.setViewportSize({ width, height: 1000 });
    // Fumadocs switches its sidebar through matchMedia after the resize.
    // Wait for readable content width, rather than capturing the old grid.
    await page.waitForFunction(() => {
      const overview = document.querySelector(".docs-overview");
      return overview && overview.getBoundingClientRect().width >= 240;
    });
    await page.evaluate(() => window.scrollTo(0, 0));
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no overflow at ${width}px`);
    assert.equal(await page.locator(".docs-overview").count(), 1, "one hydrated overview");
    assert.ok(await page.evaluate(() => document.documentElement.scrollHeight < 6000), `bounded page height at ${width}px`);
    await page.screenshot({ path: `${artifacts}/overview-${width}.png`, fullPage: true });
    assert.ok(await page.evaluate(() => document.documentElement.scrollHeight < 6000), `screenshot does not alter layout at ${width}px`);
  }
  await page.setViewportSize({ width: 1280, height: 1000 });
  for (const theme of ["light", "dark"]) {
    await page.getByRole("combobox", { name: "Reading theme" }).selectOption(theme);
    await page.evaluate(() => window.scrollTo(0, 0));
    await page.screenshot({ path: `${artifacts}/overview-${theme}.png`, fullPage: true });
  }
  await verifyPageCopy(page, base, "/consumers/getting-started");
  await verifyPageCopy(page, base, "/wire/l1");
  await page.route("**/wire/l1.md", (route) => route.fulfill({ status: 503, body: "unavailable" }));
  await page.getByRole("button", { name: "Copy page for agent" }).click();
  await page.locator("[data-page-actions] [role=status]").filter({ hasText: "Open Markdown" }).waitFor();
  assert.ok(await page.getByRole("button", { name: "Copy page for agent" }).isEnabled(), "failure allows retry");
  assert.deepEqual(errors, [], "no browser exceptions");
  await context.close();

  const staticContext = await browser.newContext({ javaScriptEnabled: false });
  const staticPage = await staticContext.newPage();
  await staticPage.goto(`${base}/overview`);
  assert.ok(await staticPage.locator(".agent-prompt pre").isVisible());
  assert.equal(await staticPage.getByRole("link", { name: "Open Markdown" }).getAttribute("href"), "/overview.md");
  const markdown = await staticContext.request.get(`${base}/overview.md`);
  assert.ok(markdown.headers()["content-type"]?.includes("text/markdown"));
  assert.ok((await markdown.text()).includes(AGENT_START.prompt));
  await staticPage.getByRole("link", { name: "Open Markdown" }).click();
  await staticPage.waitForURL(`${base}/overview.md`, { timeout: 5000 });
  await staticContext.close();
  console.log(`PASS docs (${useWebKit ? "WebKit, no pre-granted clipboard permissions" : "Chrome"}): ${count} Markdown pages, discovery links/digests, page and prompt copy, failure recovery, light/dark, 320/390/768/1280px, no-JS. Screenshots: ${artifacts}`);
} finally {
  await browser.close();
  server.stop(true);
}
