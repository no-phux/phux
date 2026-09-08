#!/usr/bin/env bun
/**
 * capture-poster.ts — refresh public/demo-poster.png from a REAL session.
 *
 * Drives the landing's live terminal island in headless Chrome: launch the
 * demo, run `demo all` in the jail, screenshot the canvas at 2x. The poster is
 * therefore an actual frame of the phux-web client rendering the wire — the
 * exact pixels the live demo shows — so the fallback never lies.
 *
 * Usage:
 *   PUBLIC_PHUX_DEMO_WS=wss://phux-demo.phalldev.workers.dev/session bun run dev   # terminal 1
 *   bun run scripts/capture-poster.ts [http://localhost:4321]                       # terminal 2
 *
 * Needs Chrome installed (playwright-core channel:"chrome" — no browser download).
 */
import { chromium } from "playwright-core";

const BASE = process.argv[2] ?? "http://localhost:4321";
const OUT = new URL("../public/demo-poster.png", import.meta.url).pathname;

const browser = await chromium.launch({ channel: "chrome", headless: true });
try {
  const page = await browser.newPage({
    viewport: { width: 1280, height: 900 },
    deviceScaleFactor: 2,
  });
  await page.goto(BASE, { waitUntil: "networkidle" });

  await page.click(".pterm-launch", { timeout: 15_000 });
  await page.waitForSelector('.pterm[data-status="live"]', { timeout: 30_000 });

  // Let the MOTD land, then run the curated gestures.
  await page.waitForTimeout(2_500);
  const canvas = page.locator(".pterm-canvas");
  await canvas.click();
  await page.keyboard.type("demo all", { delay: 40 });
  await page.keyboard.press("Enter");
  await page.waitForTimeout(4_000);

  await canvas.screenshot({ path: OUT });
  console.log(`poster written: ${OUT}`);
} finally {
  await browser.close();
}
