#!/usr/bin/env bun
/** Real WASM + real public edge acceptance. Build first:
 * PUBLIC_PHUX_DEMO_WS=wss://shell.phux.sh/session bun run build
 * bun scripts/verify-terminal-live.ts
 * After shipping: bun scripts/verify-terminal-live.ts --production (no static substitutions)
 * Static HTTP requests use the local dist server under the allowed phux.sh
 * browser origin; WebSockets go directly to the public edge. No WASM/wire mocks,
 * native auth credentials, installed phux, production local socket, or deployment.
 */
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { extname, resolve } from "node:path";
import { chromium, type BrowserContext, type Page } from "playwright-core";
import { parseTerminalPayload } from "./phux-wire";

const production = process.argv.includes("--production");
const root = resolve(import.meta.dir, "../dist");
const artifacts = resolve(import.meta.dir, "../.home-check/live-terminal");
const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
  const pathname = new URL(request.url).pathname;
  let path = resolve(root, `.${pathname}`);
  if (!path.startsWith(`${root}/`) && path !== root) return new Response(null, { status: 403 });
  if (pathname === "/") path = resolve(root, "index.html");
  if (!extname(path)) path += ".html";
  const file = Bun.file(path);
  if (!(await file.exists())) return new Response(null, { status: 404 });
  return new Response(file);
} });
const browser = await chromium.launch({ channel: "chrome", headless: true });
const errors: string[] = [];
const results: unknown[] = [];

async function geometry(page: Page, dpr: number, width: number) {
  const measured = await page.locator("canvas").evaluate((canvas: HTMLCanvasElement) => {
    const rect = canvas.getBoundingClientRect();
    const host = canvas.parentElement!;
    const frame = canvas.closest(".pterm")!;
    const header = frame.querySelector(".pterm-head")!;
    const pixels = canvas.getContext("2d")!.getImageData(0, 0, canvas.width, canvas.height).data;
    const colors = new Set<number>();
    for (let i = 0; i < pixels.length; i += 4) colors.add((pixels[i] << 16) | (pixels[i + 1] << 8) | pixels[i + 2]);
    return { cssWidth: rect.width, cssHeight: rect.height, backingWidth: canvas.width, backingHeight: canvas.height,
      cellWidth: Number(canvas.dataset.phuxCellWidth), cellHeight: Number(canvas.dataset.phuxCellHeight),
      hostWidth: host.clientWidth, frameWidth: frame.clientWidth, frameScrollWidth: frame.scrollWidth,
      headerHeight: header.getBoundingClientRect().height, focused: canvas.parentElement!.contains(document.activeElement), colors: colors.size,
      pageFits: document.documentElement.scrollWidth <= innerWidth };
  });
  assert.ok(measured.cellWidth > 8, "uses renderer's measured cells, not the old 8px estimate");
  assert.equal(measured.cellHeight, 16);
  assert.ok(measured.cssWidth <= measured.hostWidth, `canvas fits host at ${width}`);
  assert.equal(measured.cssWidth, Math.min(110, Math.floor(measured.hostWidth / measured.cellWidth)) * measured.cellWidth, "measured grid fills the available width without CSS scaling");
  assert.ok(measured.headerHeight <= 54, "one compact header row");
  assert.ok(measured.frameScrollWidth <= measured.frameWidth, "frame has no horizontal overflow");
  assert.ok(measured.pageFits, "page has no horizontal overflow");
  assert.ok(Math.abs(measured.backingWidth - measured.cssWidth * dpr) <= 1, "DPR-aware backing width");
  assert.ok(Math.abs(measured.backingHeight - measured.cssHeight * dpr) <= 1, "DPR-aware backing height");
  assert.ok(measured.colors > 8, "real ANSI terminal glyphs are painted");
  results.push({ width, dpr, ...measured });
}

async function assertTouchTargets(page: Page) {
  const targets = await page.locator(".pterm-keys button:visible, .pterm-pane-controls button").evaluateAll((buttons) => buttons.map((button) => {
    const rect = button.getBoundingClientRect();
    const frame = button.closest(".pterm")!.getBoundingClientRect();
    return { name: button.textContent, width: rect.width, height: rect.height, inside: rect.left >= frame.left && rect.right <= frame.right };
  }));
  assert.ok(targets.every((target) => target.width >= 44 && target.height >= 44 && target.inside), `all pane and touch keys have visible 44px targets: ${JSON.stringify(targets)}`);
}

async function splitAndFocus(page: Page, dpr: number) {
  const canvas = page.locator("canvas");
  const first = await canvas.getAttribute("data-phux-focused-pane");
  await page.getByRole("button", { name: "Split pane left/right", exact: true }).click();
  await page.locator('canvas[data-phux-pane-count="2"]').waitFor();
  await page.screenshot({ path: `${artifacts}/vertical-dpr${dpr}.png`, fullPage: true });
  const second = await canvas.getAttribute("data-phux-focused-pane");
  assert.notEqual(second, first);
  await canvas.click({ position: { x: 12, y: 12 } });
  await page.waitForFunction((pane) => document.querySelector("canvas")?.getAttribute("data-phux-focused-pane") === pane, first);
  const width = await canvas.evaluate((node) => node.getBoundingClientRect().width);
  await canvas.click({ position: { x: width - 12, y: 12 } });
  await page.waitForFunction((pane) => document.querySelector("canvas")?.getAttribute("data-phux-focused-pane") === pane, second);
  await page.getByRole("button", { name: "Split top/bottom", exact: true }).click();
  await page.locator('canvas[data-phux-pane-count="3"]').waitFor();
  await page.screenshot({ path: `${artifacts}/horizontal-dpr${dpr}.png`, fullPage: true });
  assert.notEqual(await page.locator("canvas").getAttribute("data-phux-focused-pane"), second);
  const third = await page.locator("canvas").getAttribute("data-phux-focused-pane");
  await page.getByRole("button", { name: "Next pane", exact: true }).click();
  await page.waitForFunction((previous) => document.querySelector("canvas")?.getAttribute("data-phux-focused-pane") !== previous, third);
  await page.getByRole("button", { name: "Close pane", exact: true }).click();
  await page.locator('canvas[data-phux-pane-count="2"]').waitFor();
  assert.ok(await page.locator("canvas").evaluate((canvas) => canvas.parentElement!.contains(document.activeElement)), "pane controls restore focus");
}

async function useLocalStaticSite(context: BrowserContext) {
  if (production) return;
  // Keep the worker's allowed browser Origin; never replace WS frames.
  await context.route("https://phux.sh/**", async (route) => {
    const url = new URL(route.request().url());
    const response = await route.fetch({ url: `http://127.0.0.1:${server.port}${url.pathname}${url.search}` });
    await route.fulfill({ response });
  });
}

async function assertCleanStartup(page: Page, output: string) {
  assert.equal(await page.locator("#phux-path-picker").count(), 0, "hosted demo never installs unsupported host controls");
  assert.equal(await page.locator(".pterm-pane-status").innerText(), "", "no unsupported-operation warning on first open");
  if (production) assert.ok(output.includes("Welcome to phux."), "deployed worker serves the new concise welcome");
}

async function scenario(dpr: number) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 1000 }, deviceScaleFactor: dpr, hasTouch: true, isMobile: true, serviceWorkers: "block" });
  await useLocalStaticSite(context);
  const page = await context.newPage();
  page.on("pageerror", (error) => errors.push(error.message));
  let output = "";
  let binaryFrames = 0;
  const decoder = new TextDecoder();
  page.on("websocket", (socket) => {
    assert.ok(socket.url().startsWith("wss://shell.phux.sh/session"), "real public edge endpoint");
    socket.on("framereceived", ({ payload }) => {
      if (typeof payload === "string") return;
      binaryFrames++;
      const terminal = parseTerminalPayload(payload);
      if (terminal) output += decoder.decode(terminal.bytes);
    });
  });
  try {
    await page.goto("https://phux.sh/");
    await page.getByRole("link", { name: "Open terminal demo", exact: true }).click();
    await page.locator('.pterm[data-status="live"]').waitFor({ timeout: 30_000 });
    assert.ok(await page.locator("canvas").evaluate((canvas) => canvas.parentElement!.contains(document.activeElement)), "ready canvas receives focus");
    await geometry(page, dpr, 1280);
    await assertCleanStartup(page, output);
    const before = output.length;
    await page.keyboard.type(`echo PHUX_BROWSER_DPR${dpr}`);
    await page.keyboard.press("Enter");
    await page.waitForTimeout(1000);
    assert.ok(output.slice(before).includes(`\nPHUX_BROWSER_DPR${dpr}`), "real command output arrives, not just echoed input");
    assert.ok(binaryFrames > 0, "real binary phux protocol frames received");
    const canvasId = await page.locator("canvas").getAttribute("id");
    await splitAndFocus(page, dpr);
    for (const width of [320, 390, 768, 1280]) {
      await page.setViewportSize({ width, height: 1000 });
      await page.waitForTimeout(250);
      await geometry(page, dpr, width);
      await assertTouchTargets(page);
      if (width <= 390) assert.equal(await page.locator(".pterm-keys button:visible").count(), 8, "all touch keys stay visible on narrow screens");
      await page.screenshot({ path: `${artifacts}/workspace-dpr${dpr}-${width}.png`, fullPage: true });
    }
    assert.equal(await page.locator("canvas").getAttribute("id"), canvasId, "splits and responsive layout preserve the same canvas DOM node");
    await page.locator(".pterm-info summary").click();
    assert.ok(await page.getByRole("button", { name: "Release session", exact: true }).isVisible());
    await page.screenshot({ path: `${artifacts}/info-dpr${dpr}.png`, fullPage: true });
    await page.getByRole("button", { name: "Release session", exact: true }).click();
    await page.locator('.pterm[data-status="idle"]').waitFor();
    await page.getByRole("button", { name: "Close live terminal", exact: true }).click();
  } catch (error) {
    await page.screenshot({ path: `${artifacts}/failure-dpr${dpr}.png`, fullPage: true });
    console.log(await page.evaluate(() => ({ coarse: matchMedia("(pointer: coarse)").matches, status: document.querySelector(".pterm")?.getAttribute("data-status"), active: document.activeElement?.outerHTML })));
    throw error;
  } finally {
    await context.close();
  }
}

try {
  await mkdir(artifacts, { recursive: true });
  await scenario(1);
  await scenario(2);
  assert.deepEqual(errors, [], "no browser exceptions");
  await writeFile(`${artifacts}/geometry.json`, JSON.stringify(results, null, 2));
  console.log(`PASS ${production ? "production (no substitutions)" : "local site/real public edge"}: real WASM focus, command output, mouse pane focus, vertical/horizontal splits, next/close, resize, measured CSS fit, DPR 1/2 backing, touch targets, Info/release, no startup warnings; 320/390/768/1280. Screenshots: ${artifacts}`);
} finally {
  await browser.close();
  server.stop(true);
}
