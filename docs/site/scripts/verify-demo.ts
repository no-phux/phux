#!/usr/bin/env bun
/** Browser acceptance against a built site and a real hosted edge Worker.
 * Usage: bun run verify:demo http://127.0.0.1:4330
 * Build with PUBLIC_PHUX_DEMO_WS pointing at your isolated Worker first.
 * Rejection/stalled-upgrade cases deliberately use controlled WebSockets;
 * the live path uses the actual configured backend and binary phux client.
 */
import { chromium } from "playwright-core";
import type { Page } from "playwright-core";
import { setTimeout as delay } from "node:timers/promises";

const BASE = process.argv[2] ?? "http://127.0.0.1:4330";
const browser = await chromium.launch({ channel: "chrome", headless: true });
let failures = 0;
function report(name: string, ok: boolean, detail = "") {
  console.log(
    `${ok ? "PASS" : "FAIL"}  ${name}${detail ? ` — ${detail}` : ""}`,
  );
  if (!ok) failures++;
}
async function openTerminal(page: Page) {
  await page.getByRole("button", { name: "Open a live terminal" }).click();
}
try {
  const noJs = await browser.newContext({ javaScriptEnabled: false });
  const staticPage = await noJs.newPage();
  await staticPage.goto(BASE);
  report(
    "no JS: diagram and install path remain usable",
    (await staticPage.locator(".mux-server").isVisible()) &&
      (await staticPage.locator('a[href="#install"]').isVisible()),
  );
  report(
    "no JS: fallback offers a working installation guide",
    await staticPage
      .locator(
        '.mux-showcase noscript a[href="https://docs.phux.sh/quickstart"]',
      )
      .isVisible(),
  );
  await noJs.close();

  const failureContext = await browser.newContext();
  const failurePage = await failureContext.newPage();
  await failurePage.routeWebSocket(/\/session/, (socket) =>
    socket.close({ code: 4001, reason: "capacity" }),
  );
  await failurePage.goto(BASE);
  await openTerminal(failurePage);
  await failurePage.locator('.pterm[data-status="closed"]').waitFor();
  await failurePage.getByRole("button", { name: "Return to controls" }).click();
  report(
    "capacity refusal recovers to launch controls",
    await failurePage.locator('.pterm[data-status="idle"]').isVisible(),
  );
  await failureContext.close();

  const cancelContext = await browser.newContext();
  const cancelPage = await cancelContext.newPage();
  let accepted = false;
  let cancelled = false;
  await cancelPage.routeWebSocket(/\/session/, (socket) => {
    accepted = true;
    socket.onClose(() => {
      cancelled = true;
    });
    // Intentionally withhold the hosted envelope; attach never completes.
  });
  await cancelPage.goto(BASE);
  await openTerminal(cancelPage);
  await cancelPage.waitForFunction(() =>
    document.querySelector('.pterm[data-status="connecting"]'),
  );
  const waitForSocket = Date.now() + 5_000;
  while (!accepted && Date.now() < waitForSocket) await delay(20);
  await cancelPage.getByRole("button", { name: "Close live terminal" }).click();
  const cancelDeadline = Date.now() + 2_000;
  while (!cancelled && Date.now() < cancelDeadline) await delay(20);
  report(
    "closing during attach cancels the pending WebSocket",
    accepted && cancelled,
  );
  await cancelContext.close();

  const liveContext = await browser.newContext();
  const page = await liveContext.newPage();
  const requests: string[] = [];
  let sockets = 0;
  let closed = 0;
  let output = "";
  page.on("request", (request) => requests.push(request.url()));
  page.on("websocket", (socket) => {
    sockets++;
    socket.on("close", () => {
      closed++;
    });
    socket.on("framereceived", ({ payload }) => {
      output += payload.toString();
    });
  });
  await page.goto(BASE, { waitUntil: "networkidle" });
  report(
    "landing does not allocate sessions or download terminal WASM",
    sockets === 0 &&
      !requests.some((url) =>
        /\.wasm(?:\?|$)|\/auth\/session|\/healthz/.test(url),
      ),
  );
  await page.getByRole("button", { name: "Share a view", exact: true }).click();
  report(
    "share diagram preserves terminal identity",
    await page
      .locator('.mux-views [class="mux-pane"] header b')
      .allTextContents()
      .then((ids) => ids.join(",") === "terminal 01,terminal 01"),
  );
  await page.getByRole("button", { name: "Detach", exact: true }).click();
  await page.getByRole("button", { name: "Reattach same terminal" }).click();
  report(
    "diagram reattaches same resource",
    await page.locator('.mux-views[data-view="mirror"]').isVisible(),
  );
  await openTerminal(page);
  await page.locator('.pterm[data-status="live"]').waitFor({ timeout: 30_000 });
  await page.waitForFunction(
    () => document.activeElement?.tagName === "CANVAS",
  );
  report(
    "real backend reaches live with keyboard focus",
    true,
    await page.locator(".pterm-controls").innerText(),
  );
  report(
    "one session, no health or auth round trip for edge",
    sockets === 1 &&
      !requests.some((url) => /\/healthz|\/auth\/session/.test(url)),
  );
  await page.keyboard.type("echo PHUX_SHOWCASE_WIRE_OK");
  await page.keyboard.press("Enter");
  const outputDeadline = Date.now() + 5_000;
  while (
    !output.includes("\nPHUX_SHOWCASE_WIRE_OK") &&
    Date.now() < outputDeadline
  )
    await delay(20);
  report(
    "real edge command returns its output over the wire",
    output.includes("\nPHUX_SHOWCASE_WIRE_OK"),
  );
  await page.getByRole("button", { name: "Close live terminal" }).click();
  await page.waitForFunction(() =>
    document.activeElement?.classList.contains("mux-launch"),
  );
  const closeDeadline = Date.now() + 2_000;
  while (closed < 1 && Date.now() < closeDeadline) await delay(20);
  report(
    "dialog returns focus and releases session",
    closed === 1 && (await page.locator("canvas").count()) === 0,
    `closed=${closed}, canvases=${await page.locator("canvas").count()}`,
  );
  await openTerminal(page);
  await page.locator('.pterm[data-status="live"]').waitFor({ timeout: 30_000 });
  report("reopening starts a fresh usable session", sockets === 2);
  await page.getByRole("button", { name: "Close live terminal" }).click();
  await liveContext.close();

  const nativeChoice = await browser.newContext();
  const nativePage = await nativeChoice.newPage();
  await nativePage.route("**/auth/session", (route) =>
    route.fulfill({
      json: { authenticated: true, provider: "github", display: "local-test" },
    }),
  );
  await nativePage.goto(`${BASE}/?auth=success`);
  await nativePage
    .locator('dialog[open] .pterm[data-status="live"]')
    .waitFor({ timeout: 30_000 });
  report(
    "OAuth return reopens native dialog and strips query",
    !new URL(nativePage.url()).searchParams.has("auth") &&
      (await nativePage
        .locator('.mux-mode-row button[aria-pressed="true"]')
        .textContent()
        .then((text) => text?.includes("Linux") ?? false)),
  );
  await nativePage.getByRole("button", { name: "Release session" }).click();
  const explicitEdge = nativePage.waitForEvent("websocket");
  await nativePage
    .getByRole("button", { name: "Use instant edge shell" })
    .click();
  report(
    "authenticated explicit edge choice bypasses native admission",
    new URL((await explicitEdge).url()).searchParams.get("mode") === "demo",
  );
  await nativePage
    .locator('.pterm[data-status="live"]')
    .waitFor({ timeout: 30_000 });
  await nativeChoice.close();

  const embedded = await browser.newContext();
  const embed = await embedded.newPage();
  await embed.goto(`${BASE}/embed`);
  await embed.getByRole("button", { name: "Use instant edge shell" }).click();
  await embed
    .locator('.pterm[data-status="live"]')
    .waitFor({ timeout: 30_000 });
  await embed.getByRole("button", { name: "Release session" }).click();
  report(
    "standalone embed can launch and release anonymous edge",
    await embed.locator('.pterm[data-status="unlock"]').isVisible(),
  );
  await embedded.close();

  const mobile = await browser.newContext({
    viewport: { width: 375, height: 667 },
    reducedMotion: "reduce",
  });
  const phone = await mobile.newPage();
  for (const path of ["/", "/quickstart"]) {
    await phone.goto(`${BASE}${path}`);
    report(
      `mobile ${path}: no horizontal overflow`,
      await phone.evaluate(
        () =>
          document.documentElement.scrollWidth <=
          document.documentElement.clientWidth,
      ),
    );
  }
  await phone.goto(BASE);
  await openTerminal(phone);
  await phone
    .locator('.pterm[data-status="live"]')
    .waitFor({ timeout: 30_000 });
  report(
    "mobile live terminal uses native-size cells",
    await phone
      .locator("canvas")
      .evaluate(
        (canvas: HTMLCanvasElement) =>
          canvas.width <= 375 &&
          Math.abs(canvas.getBoundingClientRect().width - canvas.width) < 1,
      ),
  );
  report(
    "reduced motion removes popup animation",
    await phone
      .locator("dialog")
      .evaluate((dialog) => getComputedStyle(dialog).animationName === "none"),
  );
  await mobile.close();
} finally {
  await browser.close();
}
if (failures) process.exit(1);
console.log("verify-demo: all checks pass");
