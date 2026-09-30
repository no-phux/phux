#!/usr/bin/env bun
/**
 * capture-poster.ts — refresh public/demo-poster.png from a REAL session.
 *
 * Opens the landing's live terminal dialog in headless Chrome, runs the
 * curated `demo` command, and captures the canvas at 2x. The reusable image
 * remains an actual frame of the phux-web client, not the explanatory diagram.
 *
 * Usage:
 *   PUBLIC_PHUX_DEMO_WS=ws://127.0.0.1:8799/session bun run dev                    # terminal 1
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

  await page
    .getByRole("button", { name: "Open a live terminal" })
    .click({ timeout: 15_000 });
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
