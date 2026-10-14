#!/usr/bin/env bun
/** Homepage acceptance against built assets; never uses a production server. */
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { extname, resolve } from "node:path";
import { chromium } from "playwright-core";

const root = resolve(import.meta.dir, "../dist");
const artifacts = resolve(import.meta.dir, "../.home-check");
const types: Record<string, string> = {
  ".html": "text/html", ".js": "text/javascript", ".css": "text/css",
  ".svg": "image/svg+xml", ".woff2": "font/woff2", ".png": "image/png",
};
const server = Bun.serve({
  hostname: "127.0.0.1", port: 0,
  async fetch(request) {
    const pathname = new URL(request.url).pathname;
    let path = resolve(root, `.${pathname}`);
    if (!path.startsWith(`${root}/`) && path !== root) return new Response(null, { status: 403 });
    if (pathname === "/") path = resolve(root, "index.html");
    if (!extname(path)) path += ".html";
    const file = Bun.file(path);
    if (!(await file.exists())) return new Response(null, { status: 404 });
    return new Response(file, { headers: { "content-type": types[extname(path)] ?? file.type } });
  },
});
const base = `http://127.0.0.1:${server.port}`;
const browser = await chromium.launch({ channel: "chrome", headless: true });
try {
  await mkdir(artifacts, { recursive: true });
  const context = await browser.newContext({ permissions: ["clipboard-read", "clipboard-write"] });
  const page = await context.newPage();
  const errors: string[] = [];
  const requests: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("request", (request) => requests.push(request.url()));
  // The TestFlight form is the one homepage network write; answer it locally.
  const beta: { payload: Record<string, string> | null } = { payload: null };
  await page.route("**/api/beta", async (route) => {
    beta.payload = route.request().postDataJSON() as Record<string, string>;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ ok: true, status: "invited" }),
    });
  });
  await page.goto(base);
  assert.equal(await page.getByRole("heading", { level: 1 }).innerText(), "phux");
  assert.equal(await page.locator("#how-it-works").count(), 0, "no homepage explainer");
  assert.ok(await page.locator("#testflight form").isVisible(), "TestFlight signup is on the landing");
  assert.equal(await page.locator(".tour").count(), 1, "one product tour");
  assert.equal(await page.locator("body > header, .tagline, .tour-bar").count(), 0, "no landing top bars or tagline");
  assert.equal(await page.locator(".tour-distant-host").count(), 3, "restrained background hosts");
  const entry = page.getByRole("link", { name: "Open terminal demo", exact: true });
  await page.locator("astro-island:not([ssr]) .tour-entry").waitFor();
  await page.locator(".tour").scrollIntoViewIfNeeded();
  assert.equal(await page.locator(".tour").getAttribute("data-pace"), "quick", "opens with the quick lap");
  await page.getByRole("button", { name: "Pause tour" }).click();
  const pausedProgress = await page.locator('.tour-chapters button[aria-pressed="true"]').getAttribute("style");
  await page.waitForTimeout(300);
  assert.equal(await page.locator('.tour-chapters button[aria-pressed="true"]').getAttribute("style"), pausedProgress, "pause freezes the quick lap");
  await page.getByRole("button", { name: "Play tour" }).click();
  await page.locator('.tour[data-pace="walkthrough"]').waitFor({ timeout: 12_000 });
  assert.equal(await page.locator('.tour-chapters button[aria-pressed="true"]').innerText(), "01 Attach", "quick lap restarts as an explanatory walkthrough");
  assert.match(await page.locator(".tour-pace").innerText(), /Walkthrough/);
  // The playback checks used the mouse. Exercise actual keyboard navigation
  // before asserting :focus-visible, rather than relying on browser modality.
  await entry.focus();
  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Tab");
  await page.waitForFunction(() => getComputedStyle(document.querySelector(".tour-invitation")!).opacity === "1");
  assert.equal(await page.locator(".tour-invitation").evaluate((node) => getComputedStyle(node).opacity), "1", "keyboard focus reveals the invitation");
  assert.equal(await page.locator("dialog[open]").count(), 0, "discovery does not launch a session");
  await entry.hover();
  assert.equal(await page.locator("dialog[open]").count(), 0, "hover only prepares the client");
  const chapters = page.getByRole("group", { name: "Tour chapters" }).getByRole("button");
  assert.equal(await chapters.count(), 4, "four tour chapters");
  await chapters.nth(2).click();
  assert.equal(await chapters.nth(2).getAttribute("aria-pressed"), "true", "chapter selection");
  assert.equal(await page.locator(".tour").getAttribute("data-pace"), "walkthrough", "manual chapter selection uses readable pacing");
  await page.getByRole("button", { name: "Pause tour" }).click();
  assert.ok(await page.getByRole("button", { name: "Play tour" }).isVisible(), "tour pauses");
  assert.equal(await page.locator("main .features h2").count(), 3);
  assert.equal(await page.getByRole("link", { name: /Let agents work/ }).getAttribute("href"), "https://docs.phux.sh/consumers/getting-started");
  await page.getByRole("button", { name: "Copy install command" }).click();
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), "curl -fsSL https://phux.sh/install | sh");
  assert.equal(await page.getByRole("status").innerText(), "Install command copied.");
  // The hero switcher swaps the command on hover, focus, click, and arrows.
  const command = page.locator("#install-command");
  const target = (name: string) => page.locator(`#install [data-install-target="${name}"]`);
  await target("desktop").hover();
  assert.equal(await command.innerText(), "curl -fsSL https://phux.sh/install-desktop | sh", "hover swaps to the desktop installer");
  await target("canary").click();
  assert.equal(await target("canary").getAttribute("aria-pressed"), "true", "the clicked target is pressed");
  assert.equal(await command.innerText(), "curl -fsSL https://phux.sh/install-desktop | sh -s -- --channel next", "canary selects the desktop next channel");
  await page.getByRole("button", { name: "Copy install command" }).click();
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), "curl -fsSL https://phux.sh/install-desktop | sh -s -- --channel next", "copy follows the active target");
  await target("canary").focus();
  await page.keyboard.press("ArrowLeft");
  assert.equal(await command.innerText(), "curl -fsSL https://phux.sh/install-desktop | sh", "arrow keys move the target");
  await page.getByRole("button", { name: "Copy install command" }).click();
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), "curl -fsSL https://phux.sh/install-desktop | sh", "copy follows the keyboard target");
  // Regression: a content-sized dock widened with the command and slid the
  // targets out from under the pointer. The dock and its targets are pinned.
  const box = (selector: string) =>
    page.locator(selector).evaluate((el) => {
      const rect = el.getBoundingClientRect();
      return { x: Math.round(rect.x), y: Math.round(rect.y), w: Math.round(rect.width), h: Math.round(rect.height) };
    });
  const dock = await box("#install");
  const cliTarget = await box('#install [data-install-target="cli"]');
  for (const name of ["desktop", "canary", "cli"]) {
    await target(name).hover();
    assert.deepEqual(await box("#install"), dock, `the dock keeps its box while ${name} is previewed`);
    assert.deepEqual(await box('#install [data-install-target="cli"]'), cliTarget, `targets stay pinned while ${name} is previewed`);
  }
  // The landing carries the same TestFlight request /more posts.
  await page.locator("#beta-form input[name=email]").fill("iphone@example.com");
  await page.locator("#beta-form button[type=submit]").click();
  await page.waitForFunction(() => document.querySelector("#beta-note")?.textContent?.includes("Invite sent") ?? false);
  assert.match(await page.locator("#beta-note").innerText(), /Invite sent/, "the landing reports the invite");
  assert.equal(beta.payload?.email, "iphone@example.com", "landing signup posts the address");
  assert.equal(beta.payload?.website, "", "the honeypot stays empty");
  // Leave the longest command up for the overflow pass below.
  await target("canary").click();
  for (const width of [320, 390, 768, 1280]) {
    await page.setViewportSize({ width, height: 1000 });
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no overflow at ${width}px`);
    assert.ok(await page.locator("#install code").evaluate((code) => code.scrollWidth <= code.clientWidth), `full install command readable at ${width}px`);
    await page.screenshot({ path: `${artifacts}/home-${width}.png`, fullPage: true });
  }
  assert.ok(!requests.some((url) => /\.wasm|\/auth\/session|\/healthz|phux-architecture\.mp4/.test(url)), "idle homepage does not allocate sessions or fetch film/WASM");
  await page.getByRole("link", { name: /Try it in your browser/ }).click();
  await page.locator("dialog[open]").waitFor();
  await page.getByRole("button", { name: "Close live terminal" }).click();
  assert.equal(await page.locator("dialog[open]").count(), 0);
  assert.ok(await page.getByRole("link", { name: /Try it in your browser/ }).evaluate((link) => link === document.activeElement), "closing restores focus");
  await page.goto(`${base}/more`);
  assert.equal(await page.getByRole("link", { name: /Built for automation/ }).getAttribute("href"), "https://docs.phux.sh/consumers/getting-started");
  assert.ok(await page.locator("#testflight form").isVisible(), "iPhone signup is preserved");
  await page.locator("#testflight input[name=email]").fill("more@example.com");
  await page.locator("#testflight button[type=submit]").click();
  await page.waitForFunction(() => document.querySelector("#beta-note")?.textContent?.includes("Invite sent") ?? false);
  assert.equal(beta.payload?.email, "more@example.com", "/more still posts the TestFlight request");
  assert.ok(await page.locator("#beta form").isVisible(), "release signup is preserved");
  assert.equal(await page.locator("[data-copy]").count(), 3, "alternate installers preserved");
  await page.goto(`${base}/privacy`);
  assert.equal(await page.getByRole("link", { name: "join the beta" }).getAttribute("href"), "/more#beta");
  assert.equal(await page.getByRole("link", { name: "request access to phux for iPhone" }).getAttribute("href"), "/more#testflight");
  await page.goto(`${base}/telemetry`);
  assert.equal(await page.getByRole("link", { name: "release mailing list" }).getAttribute("href"), "/more#beta");
  assert.deepEqual(errors, [], "no browser exceptions");
  await context.close();
  const stillContext = await browser.newContext({ reducedMotion: "reduce" });
  const stillPage = await stillContext.newPage();
  await stillPage.goto(base);
  await stillPage.locator("astro-island:not([ssr]) .tour").waitFor();
  assert.equal(await stillPage.locator(".tour-play").count(), 0, "reduced motion does not autoplay");
  assert.equal(await stillPage.locator(".tour-pace").count(), 0, "reduced motion does not promise a moving quick pass");
  assert.match(await stillPage.locator(".tour-terminal").innerText(), /Finished/, "reduced motion shows the settled chapter");
  await stillContext.close();
  const staticContext = await browser.newContext({ javaScriptEnabled: false });
  const staticPage = await staticContext.newPage();
  await staticPage.goto(base);
  assert.equal(await staticPage.getByRole("heading", { level: 1 }).innerText(), "phux");
  assert.ok(await staticPage.locator("#install code").isVisible());
  assert.equal(await staticPage.getByRole("link", { name: /Try it in your browser/ }).getAttribute("href"), "/embed");
  assert.match(await staticPage.locator(".tour-terminal").innerText(), /Finished/, "no-JS tour shows a settled scene");
  await staticContext.close();
  console.log(`PASS home: copy, quick lap → walkthrough, chapters/pause/reduced motion, demo open/close/focus, 320/390/768/1280px, no-JS, secondary content, idle resource budget. Screenshots: ${artifacts}`);
} finally {
  await browser.close();
  server.stop(true);
}
