#!/usr/bin/env bun
/**
 * Browser acceptance for the demo pairing link, against a live site.
 * Usage: bun run verify:pair [https://phux.sh]
 * Before deployment: PHUX_SITE_STATIC_ORIGIN=http://127.0.0.1:4337 bun run verify:pair
 * Substitutes local built HTTP assets only; real WebSockets keep the allowed origin.
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

/** Raw terminal output this page's WebSocket has received (VT bytes as text). */
function trackedOutput(page: Page): () => string {
  let text = "";
  page.on("websocket", (socket) => {
    socket.on("framereceived", ({ payload }) => {
      text += payload.toString();
    });
  });
  return () => text;
}

/** Type at the terminal in this view, as a person would. */
async function typeIn(page: Page, text: string): Promise<void> {
  const canvas = page.locator("canvas[data-phux-pane-count]").first();
  await canvas.waitFor({ timeout: 20_000 });
  await canvas.click();
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

async function waitFor(found: () => boolean | Promise<boolean>, what: string, timeoutMs = 15_000) {
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
  try {
    await page.locator('.pterm[data-status="live"]').first().waitFor({ timeout: 30_000 });
  } catch (error) {
    // Say what the screen was actually doing instead of only "not live".
    const status = await page.locator(".pterm").first().getAttribute("data-status").catch(() => "no .pterm");
    const overlay = (await page.locator(".pterm-overlay").first().innerText().catch(() => "")).replace(/\n/g, " | ");
    const dialog = await page.locator("dialog[open]").count();
    console.error(
      `verify-pair: no live terminal — status=${status} dialog=${dialog} overlay="${overlay.slice(0, 120)}" url=${page.url()}`,
    );
    throw error;
  }
}

/**
 * Open the hosted shell from the landing page. The dialog is opened by a
 * hydrated island's document listener, so the click has to come after hydration:
 * before that the link falls through to its href (/embed, sign-in mode).
 */
async function openHostedShell(page: Page) {
  await page.goto(BASE);
  await page.locator("astro-island:not([ssr])").first().waitFor({ timeout: 20_000 });
  await page.getByRole("link", { name: /Try it in your browser/ }).click();
  await page.locator("dialog[open]").waitFor({ timeout: 10_000 });
  await liveTerminal(page);
}

async function useLocalAssets(context: BrowserContext) {
  const local = process.env.PHUX_SITE_STATIC_ORIGIN;
  if (!local) return;
  await context.route(`${BASE}/**`, async (route) => {
    const url = new URL(route.request().url());
    const response = await route.fetch({ url: `${local}${url.pathname}${url.search}` });
    await route.fulfill({ response });
  });
}

try {
  const desktop: BrowserContext = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await useLocalAssets(desktop);
  await desktop.grantPermissions(["clipboard-read", "clipboard-write"], { origin: BASE });
  const wide = await desktop.newPage();
  const desktopText = trackedOutput(wide);
  await openHostedShell(wide);
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
  report("desktop: the pairing link points at the phone page", link.startsWith(expectedPrefix));
  if (!link.startsWith(expectedPrefix)) throw new Error("no usable pairing link");
  report("desktop: the code rides in the fragment, never the query", !link.includes("?"));
  const originalQr = await qr.innerHTML();
  await wide.locator(".mux-mode-row").getByRole("button", { name: "Demo", exact: true }).click();
  report("desktop: reselecting the active runtime preserves pairing",
    await wide.locator(".mux-pair-toggle").getAttribute("aria-expanded") === "true" &&
    await qr.innerHTML() === originalQr);

  // The phone: a coarse pointer, a small viewport, no install and no sign-in.
  const phone = await browser.newContext({
    viewport: { width: 390, height: 780 },
    hasTouch: true,
    isMobile: true,
  });
  await useLocalAssets(phone);
  const handset = await phone.newPage();
  const phoneText = trackedOutput(handset);
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

  // One session, two screens: what one view types, the other view receives. The
  // panes are per view (each screen keeps its own layout over the shared
  // terminals), so terminal output — not pane count — is what crosses.
  await typeIn(handset, "echo PHUX_FROM_PHONE");
  report(
    "phone → desktop: output typed on the phone reaches the desktop shell",
    await waitFor(() => desktopText().includes("PHUX_FROM_PHONE"), "the phone's output on the desktop"),
  );
  await typeIn(wide, "echo PHUX_FROM_DESKTOP");
  report(
    "desktop → phone: output typed on the desktop reaches the phone shell",
    await waitFor(() => phoneText().includes("PHUX_FROM_DESKTOP"), "the desktop's output on the phone"),
  );

  await phone.close();
  await wide.locator('summary[aria-label="Info & shortcuts"]').click();
  await wide.getByRole("button", { name: "Release session", exact: true }).click();
  report("desktop: releasing a session retires its pairing code", await wide.locator(".mux-pair").count() === 0);
  await wide.getByRole("button", { name: "Launch edge WASM shell", exact: true }).click();
  await liveTerminal(wide);
  report("desktop: a new session starts with a collapsed, fresh pairing panel",
    await wide.getByRole("button", { name: /Pair your phone/ }).getAttribute("aria-expanded") === "false");
  await wide.getByRole("button", { name: /Pair your phone/ }).click();
  await wide.getByRole("button", { name: /Copy link/ }).click();
  const freshLink = await wide.evaluate(() => navigator.clipboard.readText());
  report("desktop: the replacement session cannot reuse the spent link", freshLink.startsWith(expectedPrefix) && freshLink !== link);
  await desktop.close();
} finally {
  await browser.close();
}
if (failures > 0) {
  console.error(`verify-pair: ${failures} check(s) failed`);
  process.exit(1);
}
console.log("verify-pair: all checks pass");
