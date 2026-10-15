#!/usr/bin/env bun
/**
 * Browser acceptance for the demo pairing link, against a live site.
 * Usage: bun run verify:pair [https://phux.sh]
 *
 * Drives the real thing: one screen opens the hosted shell, the dialog hands out
 * its pairing link, a second (phone-shaped) browser opens that link, and the two
 * screens are proven to be one session — a pane the phone spawns shows up on the
 * desktop, and back. Both contexts talk to the deployed demo Worker, so this is
 * the check that the pairing surface works end to end in the browser, which the
 * websocket smoke cannot see.
 */
import { chromium } from "playwright-core";
import type { BrowserContext, Page } from "playwright-core";

const BASE = (process.argv[2] ?? "https://phux.sh").replace(/\/$/, "");
const browser = await chromium.launch({ channel: "chrome", headless: true });
let failures = 0;
function report(name: string, ok: boolean, detail = "") {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures++;
}

/** The pane readout both screens publish: "<n> / 4 panes". */
const paneCount = (page: Page) =>
  page.locator(".pterm-pane-count").first().innerText();

async function waitFor(found: () => Promise<boolean>, what: string, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (await found()) return true;
    if (Date.now() > deadline) {
      console.error(`verify-pair: timed out waiting for ${what}`);
      return false;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
}

async function liveTerminal(page: Page) {
  await page.locator('.pterm[data-status="live"]').first().waitFor({ timeout: 30_000 });
}

try {
  const desktop: BrowserContext = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await desktop.grantPermissions(["clipboard-read", "clipboard-write"], { origin: BASE });
  const wide = await desktop.newPage();
  await wide.goto(BASE);
  await wide.getByRole("link", { name: /Try it in your browser/ }).click();
  await liveTerminal(wide);
  report("desktop: hosted shell is live", true);

  // The pairing line appears with the session envelope, before anyone asks.
  await wide.getByRole("button", { name: /Pair your phone/ }).click();
  const qr = wide.locator(".mux-pair-code svg");
  await qr.waitFor({ timeout: 10_000 });
  report("desktop: the pairing panel renders a code", await qr.isVisible());
  report(
    "desktop: the panel waits for the phone",
    /Waiting for your phone/.test(await wide.locator(".mux-pair-state").innerText()),
  );

  await wide.getByRole("button", { name: /Copy link/ }).click();
  const link = await wide.evaluate(() => navigator.clipboard.readText());
  const expectedPrefix = `${BASE}/phone#`;
  report("desktop: the pairing link points at the phone page", link.startsWith(expectedPrefix), link);
  if (!link.startsWith(expectedPrefix)) throw new Error("no usable pairing link");
  report("desktop: the code rides in the fragment, never the query", !link.includes("?"));

  // The phone: a coarse pointer, a small viewport, no install and no sign-in.
  const phone = await browser.newContext({
    viewport: { width: 390, height: 780 },
    hasTouch: true,
    isMobile: true,
  });
  const handset = await phone.newPage();
  await handset.goto(link);
  await liveTerminal(handset);
  report("phone: the paired shell renders", await handset.locator(".pterm").first().isVisible());
  report(
    "phone: the code leaves the address bar after it is spent",
    !handset.url().includes("#"),
  );
  report(
    "phone: no pairing panel on the paired view",
    (await handset.getByRole("button", { name: /Pair your phone/ }).count()) === 0,
  );

  report(
    "desktop: the session reports the phone as linked",
    await waitFor(
      async () => /Phone linked/.test(await wide.locator(".mux-pair-toggle").innerText()),
      "the linked state",
    ),
  );

  // One session, two screens: what the phone spawns, the desktop sees.
  await handset.getByRole("button", { name: "Split pane left/right" }).click();
  report(
    "phone → desktop: a pane spawned on the phone appears on the desktop",
    await waitFor(async () => (await paneCount(wide)).startsWith("2 / 4"), "the desktop pane count"),
    await paneCount(wide),
  );

  await wide.getByRole("button", { name: "Split pane left/right" }).click();
  report(
    "desktop → phone: a pane spawned on the desktop appears on the phone",
    await waitFor(async () => (await paneCount(handset)).startsWith("3 / 4"), "the phone pane count"),
    await paneCount(handset),
  );

  await phone.close();
  await desktop.close();
} finally {
  await browser.close();
}
if (failures > 0) {
  console.error(`verify-pair: ${failures} check(s) failed`);
  process.exit(1);
}
console.log("verify-pair: all checks pass");
