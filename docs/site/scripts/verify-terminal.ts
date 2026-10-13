#!/usr/bin/env bun
/** Deterministic UI acceptance, not a live wire/backend test.
 * PUBLIC_PHUX_DEMO_WS=wss://terminal.invalid/session bun run build
 * bun scripts/verify-terminal.ts
 * The browser stubs WASM and /auth/session; real wire/input/provider behavior
 * is not exercised by these presentation checks.
 */
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { extname, resolve } from "node:path";
import { chromium, type BrowserContext, type Page } from "playwright-core";

declare global {
  interface Window {
    __terminalScenario?: string;
    __terminalFixture?: { starts: number; closes: number; frees: number; aborted: number; actions: string[]; mode: string };
  }
}
const root = resolve(import.meta.dir, "../dist");
const artifacts = resolve(import.meta.dir, "../.home-check");
const mime: Record<string, string> = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml" };
const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
  const pathname = new URL(request.url).pathname;
  let path = resolve(root, `.${pathname}`);
  if (!path.startsWith(`${root}/`) && path !== root) return new Response(null, { status: 403 });
  if (pathname === "/") path = resolve(root, "index.html");
  if (!extname(path)) path += ".html";
  const file = Bun.file(path);
  if (!(await file.exists())) return new Response(null, { status: 404 });
  return new Response(file, { headers: { "content-type": mime[extname(path)] ?? file.type } });
} });
const base = `http://127.0.0.1:${server.port}`;
const browser = await chromium.launch({ channel: "chrome", headless: true });
const errors: string[] = [];

async function context(scenario = "live", authenticated = false, mobile = false) {
  const ctx = await browser.newContext({ viewport: { width: mobile ? 390 : 1280, height: mobile ? 844 : 1000 }, hasTouch: mobile, isMobile: mobile });
  await ctx.addInitScript((value) => { window.__terminalScenario = value; }, scenario);
  await ctx.route(/\/_astro\/phux_web\.[^/]+\.js$/, (route) => route.fulfill({ contentType: "text/javascript", path: resolve(import.meta.dir, "fixtures/terminal-ui.js") }));
  await ctx.route("**/auth/session", (route) => route.fulfill({ json: authenticated ? { authenticated: true, provider: "github", display: "ui-test" } : { authenticated: false } }));
  ctx.on("page", (page) => page.on("pageerror", (error) => errors.push(error.message)));
  return ctx;
}
async function open(page: Page) {
  await page.goto(base);
  assert.equal(await page.evaluate(() => window.__terminalFixture?.starts ?? 0), 0, "idle page allocates nothing");
  await page.getByRole("link", { name: "Open terminal demo", exact: true }).focus();
  await page.keyboard.press("Enter");
  await page.locator("dialog[open]").waitFor();
}
async function live(page: Page, expectFocus = true) {
  await page.locator('.pterm[data-status="live"]').waitFor();
  if (expectFocus) assert.ok(await page.locator("canvas").evaluate((canvas) => canvas === document.activeElement), "live terminal receives keyboard focus");
  assert.ok(await page.locator("canvas").evaluate((canvas) => canvas.getBoundingClientRect().width <= canvas.parentElement!.clientWidth), "measured 9px geometry fits before the first live paint");
  assert.equal(await page.locator(".pterm-head").count(), 1, "one terminal header");
}
async function widths(page: Page, name: string) {
  for (const width of [320, 390, 768, 1280]) {
    await page.setViewportSize({ width, height: 1000 });
    await page.waitForTimeout(100);
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `${name}: no page overflow at ${width}`);
    assert.ok(await page.locator(".pterm").evaluate((term) => term.scrollWidth <= term.clientWidth), `${name}: no frame overflow at ${width}`);
    assert.ok(await page.locator("canvas").evaluate((canvas: HTMLCanvasElement) => Math.abs(canvas.width - canvas.getBoundingClientRect().width) < 1), "canvas stays native-sized, not CSS-scaled");
    await page.screenshot({ path: `${artifacts}/${name}-${width}.png`, fullPage: true });
  }
}
async function paneControls(page: Page) {
  await page.getByRole("button", { name: "Split pane left/right", exact: true }).click();
  await page.locator('canvas[data-phux-pane-count="2"]').waitFor();
  await page.getByRole("button", { name: "Split top/bottom", exact: true }).click();
  await page.locator('canvas[data-phux-pane-count="3"]').waitFor();
  await page.getByRole("button", { name: "Next pane", exact: true }).click();
  await page.getByRole("button", { name: "Split pane left/right", exact: true }).click();
  await page.locator('canvas[data-phux-pane-count="4"]').waitFor();
  assert.ok(await page.getByRole("button", { name: "Split pane left/right", exact: true }).isDisabled());
  assert.ok(await page.getByRole("button", { name: "Split top/bottom", exact: true }).isDisabled());
  await page.getByRole("button", { name: "Close pane", exact: true }).click();
  await page.locator('canvas[data-phux-pane-count="3"]').waitFor();
  assert.deepEqual(await page.evaluate(() => window.__terminalFixture?.actions), ["split:vertical", "split:horizontal", "next", "split:vertical", "close-pane"]);
  assert.ok(await page.locator("canvas").evaluate((canvas) => canvas === document.activeElement), "pane actions return input focus");
}
async function info(page: Page) {
  assert.ok(!(await page.getByRole("complementary", { name: "Session details" }).isVisible()), "details start quiet");
  const summary = page.locator(".pterm-info summary");
  await summary.focus();
  await page.keyboard.press("Enter");
  assert.ok(await page.getByRole("complementary", { name: "Session details" }).isVisible());
  assert.ok(await page.getByRole("complementary", { name: "Terminal pane shortcuts" }).isVisible());
  await page.keyboard.press("Space");
  assert.equal(await page.locator(".pterm-info").getAttribute("open"), null, "keyboard closes details");
}
async function close(page: Page) {
  await page.keyboard.press("Escape");
  assert.equal(await page.locator("dialog[open]").count(), 0);
  assert.ok(await page.getByRole("link", { name: "Open terminal demo", exact: true }).evaluate((link) => link === document.activeElement), "Escape returns focus to preview");
}
async function dispose(ctx: BrowserContext) { await ctx.close(); }

try {
  await mkdir(artifacts, { recursive: true });
  const demo = await context();
  const page = await demo.newPage();
  await open(page);
  await live(page);
  const colors = await page.locator(".terminal-surface").evaluateAll((nodes) => nodes.map((node) => getComputedStyle(node).backgroundColor));
  assert.ok(colors.every((color) => color === "rgb(11, 11, 13)"), "preview, dialog and shell share one surface");
  await info(page);
  await paneControls(page);
  await widths(page, "terminal");
  await page.emulateMedia({ reducedMotion: "reduce" });
  assert.equal(await page.locator("dialog").evaluate((node) => getComputedStyle(node).animationName), "none");
  await close(page);
  assert.equal(await page.evaluate(() => window.__terminalFixture?.closes), 1, "close releases the session once");
  await dispose(demo);

  const phone = await context("live", false, true);
  const mobile = await phone.newPage();
  await open(mobile);
  await live(mobile);
  assert.ok(await mobile.getByRole("toolbar", { name: "Terminal keys" }).isVisible());
  for (const width of [320, 390]) {
    await mobile.setViewportSize({ width, height: 844 });
    await mobile.waitForTimeout(100);
    const targets = await mobile.locator(".pterm-keys button").evaluateAll((nodes) => nodes.map((node) => {
      const rect = node.getBoundingClientRect();
      const bar = node.parentElement!.getBoundingClientRect();
      return { width: rect.width, height: rect.height, inside: rect.left >= bar.left && rect.right <= bar.right };
    }));
    assert.ok(targets.every((target) => target.width >= 44 && target.height >= 44 && target.inside), `all touch keys visible with real 44px targets at ${width}px`);
  }
  await mobile.getByRole("button", { name: "Ctrl", exact: true }).click();
  assert.equal(await mobile.getByRole("button", { name: "Ctrl", exact: true }).getAttribute("aria-pressed"), "true");
  await mobile.screenshot({ path: `${artifacts}/terminal-touch.png`, fullPage: true });
  await dispose(phone);

  const auth = await context();
  const authPage = await auth.newPage();
  await authPage.goto(`${base}/embed`);
  await authPage.locator('.pterm[data-status="unlock"]').waitFor();
  assert.ok(await authPage.getByRole("navigation", { name: "Terminal navigation" }).isVisible());
  for (const width of [320, 390, 768, 1280]) {
    await authPage.setViewportSize({ width, height: 1000 });
    assert.ok(await authPage.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    await authPage.screenshot({ path: `${artifacts}/terminal-signin-${width}.png`, fullPage: true });
  }
  const screenBefore = await authPage.locator(".pterm-screen").evaluate((screen) => screen.getBoundingClientRect().height);
  await authPage.getByRole("button", { name: "Use instant edge shell" }).click();
  await live(authPage, false);
  assert.equal(await authPage.locator(".pterm-screen").evaluate((screen) => screen.getBoundingClientRect().height), screenBefore, "screen space stays stable from sign-in to live");
  await widths(authPage, "terminal-standalone");
  await authPage.locator(".pterm-info summary").click();
  await authPage.getByRole("button", { name: "Release session" }).click();
  await authPage.locator('.pterm[data-status="unlock"]').waitFor();
  await authPage.goto(`${base}/?auth=error&auth_error=configuration`);
  await authPage.locator('dialog[open] [role="alert"]').waitFor();
  assert.equal(new URL(authPage.url()).search, "", "OAuth callback query is removed");
  await authPage.getByRole("button", { name: "Demo", exact: true }).click();
  await live(authPage);
  assert.equal(await authPage.locator('[role="alert"]').count(), 0, "explicit demo choice clears obsolete sign-in error");
  await dispose(auth);

  const stalled = await context("stalled");
  const waiting = await stalled.newPage();
  await open(waiting);
  await waiting.locator('.pterm[data-status="connecting"]').waitFor();
  await waiting.waitForFunction(() => (window.__terminalFixture?.starts ?? 0) > 0);
  await close(waiting);
  assert.equal(await waiting.evaluate(() => window.__terminalFixture?.aborted), 1, "closing cancels pending attach");
  await dispose(stalled);

  const rejection = await context("rejected");
  const refused = await rejection.newPage();
  await open(refused);
  await refused.locator('.pterm[data-status="closed"]').waitFor();
  assert.ok((await refused.locator(".pterm-retry").innerText()).includes("busy"));
  await refused.getByRole("button", { name: "Return to controls" }).click();
  await refused.locator('.pterm[data-status="idle"]').waitFor();
  await dispose(rejection);

  const fallback = await context("fallback", true);
  const limited = await fallback.newPage();
  await limited.goto(`${base}/?auth=success`);
  await live(limited);
  assert.ok(await limited.locator(".pterm-fallback").isVisible(), "fallback remains visible even with Info collapsed");
  assert.ok((await limited.locator(".pterm-fallback").innerText()).includes("capacity"));
  await dispose(fallback);

  assert.deepEqual(errors, [], "no browser exceptions");
  console.log(`PASS terminal UI fixture: shared surfaces, keyboard launch/focus/Escape, panes/cap, info disclosure, mobile touch targets, 320/390/768/1280px, native-sized canvas, sign-in/OAuth, rejection/recovery, pending cancellation, visible fallback, reduced motion. Screenshots: ${artifacts}`);
} finally {
  await browser.close();
  server.stop(true);
}
