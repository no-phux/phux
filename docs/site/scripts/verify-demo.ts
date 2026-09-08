#!/usr/bin/env bun
/**
 * verify-demo.ts — the hero's failure-mode battery, against a built preview.
 *
 * Checks the brief's "always render something" bar:
 *   1. JS off            → poster visible, launch button hidden, page coherent
 *   2. backend blocked   → click launch → "demo unreachable" + poster stays
 *   3. backend reachable → click launch → live canvas
 *   4. 375px viewport    → no horizontal scroll on / and /quickstart
 *
 * Usage: bun run scripts/verify-demo.ts [http://localhost:4330]
 */
import { chromium } from "playwright-core";

const BASE = process.argv[2] ?? "http://localhost:4330";
let failures = 0;

function report(name: string, ok: boolean, detail = "") {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures++;
}

const browser = await chromium.launch({ channel: "chrome", headless: true });

// 1. JS off
{
  const ctx = await browser.newContext({ javaScriptEnabled: false });
  const page = await ctx.newPage();
  await page.goto(BASE, { waitUntil: "load" });
  const posterVisible = await page.locator(".pterm-poster").isVisible();
  const launchVisible = await page.locator(".pterm-launch").isVisible().catch(() => false);
  const h1 = (await page.locator("h1").first().textContent())?.trim() ?? "";
  report("no-JS: poster renders", posterVisible);
  report("no-JS: launch button hidden", !launchVisible);
  report("no-JS: thesis present", h1.length > 0, h1);
  await ctx.close();
}

// 2. backend unreachable → honest fallback
{
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await page.route("**/healthz", (r) => r.abort());
  await page.goto(BASE, { waitUntil: "networkidle" });
  await page.click(".pterm-launch");
  const label = page.locator(".pterm-label");
  await label.waitFor({ timeout: 10_000 });
  const text = (await label.textContent()) ?? "";
  report("backend down: honest fallback", /unreachable/.test(text), text.trim());
  report("backend down: poster still covers", await page.locator(".pterm-poster").isVisible());
  await ctx.close();
}

// 3. live path
{
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await page.goto(BASE, { waitUntil: "networkidle" });
  await page.click(".pterm-launch");
  const live = await page
    .waitForSelector('.pterm[data-status="live"]', { timeout: 30_000 })
    .then(() => true)
    .catch(() => false);
  report("backend up: goes live on launch", live);
  await ctx.close();
}

// 4. mobile: no horizontal scroll
for (const path of ["/", "/quickstart"]) {
  const ctx = await browser.newContext({ viewport: { width: 375, height: 667 } });
  const page = await ctx.newPage();
  await page.goto(`${BASE}${path}`, { waitUntil: "load" });
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  report(`mobile ${path}: no horizontal scroll`, overflow <= 0, `overflow ${overflow}px`);
  await ctx.close();
}

await browser.close();
if (failures) {
  console.error(`verify-demo: ${failures} failure(s)`);
  process.exit(1);
}
console.log("verify-demo: all checks pass");
