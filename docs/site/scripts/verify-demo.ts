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
  await page.getByRole("link", { name: "Try it in your browser" }).click();
}

async function waitForOutput(found: () => boolean) {
  const deadline = Date.now() + 5_000;
  while (!found() && Date.now() < deadline) await delay(20);
  if (!found()) throw new Error("Expected terminal output did not arrive");
}
try {
  const noJs = await browser.newContext({ javaScriptEnabled: false });
  const staticPage = await noJs.newPage();
  await staticPage.goto(BASE);
  report(
    "no JS: demo link falls back to the standalone shell",
    (await staticPage
      .getByRole("link", { name: "Try it in your browser" })
      .getAttribute("href")) === "/embed" &&
      (await staticPage.locator('a[href="#install"]').isVisible()),
  );
  report(
    "no JS: fallback offers a working installation guide",
    await staticPage
      .locator(
        '.hero-demo noscript a[href="https://docs.phux.sh/quickstart"]',
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
  await openTerminal(page);
  await page.locator('.pterm[data-status="live"]').waitFor({ timeout: 30_000 });
  await page.waitForFunction(() =>
    document.activeElement?.classList.contains("phux-web-input"),
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

  // A half-typed command must stay in its terminal while another pane runs.
  await page.keyboard.type("echo PHUX_LEFT_");
  await page
    .getByRole("button", { name: "Split left/right", exact: true })
    .click();
  await page.locator('canvas[data-phux-pane-count="2"]').waitFor();
  await page.getByRole("button", { name: "Next pane", exact: true }).waitFor();
  await page.waitForFunction(
    () =>
      !document.querySelector<HTMLButtonElement>(
        ".pterm-pane-controls button:nth-of-type(3)",
      )?.disabled,
  );
  await page.keyboard.type("echo PHUX_RIGHT_OK");
  await page.keyboard.press("Enter");
  await waitForOutput(() => output.includes("\nPHUX_RIGHT_OK"));
  await page.getByRole("button", { name: "Next pane", exact: true }).click();
  await page.keyboard.type("OK");
  await page.keyboard.press("Enter");
  await waitForOutput(() => output.includes("\nPHUX_LEFT_OK"));
  report(
    "split panes preserve independent command lines over one connection",
    sockets === 1,
  );

  await page.keyboard.press("Control+a");
  await page.keyboard.type("%");
  await page.locator('canvas[data-phux-pane-count="3"]').waitFor();
  await page.waitForFunction(
    () =>
      !document.querySelector<HTMLButtonElement>(
        ".pterm-pane-controls button:nth-of-type(3)",
      )?.disabled,
  );
  await page.keyboard.press("Control+a");
  await page.keyboard.type('"');
  await page.locator('canvas[data-phux-pane-count="4"]').waitFor();
  report(
    "both keyboard split directions work and enforce the pane cap",
    (await page
      .getByRole("button", { name: "Split left/right", exact: true })
      .isDisabled()) &&
      (await page
        .getByRole("button", { name: "Split top/bottom", exact: true })
        .isDisabled()),
  );
  const desktopViewport = page.viewportSize()!;
  await page.setViewportSize({ width: 375, height: 812 });
  await page.waitForFunction(() => {
    const canvas = document.querySelector("canvas");
    const host = document.querySelector(".pterm-canvas-host");
    return canvas && host && canvas.width <= host.clientWidth;
  });
  report(
    "resizing a live four-pane view keeps every pane inside the narrow canvas",
    await page.locator('canvas[data-phux-pane-count="4"]').isVisible(),
  );
  await page.setViewportSize(desktopViewport);
  await page.waitForFunction(
    () => (document.querySelector("canvas")?.width ?? 0) > 375,
  );
  for (const count of [3, 2, 1]) {
    await page.getByRole("button", { name: "Close pane", exact: true }).click();
    await page.locator(`canvas[data-phux-pane-count="${count}"]`).waitFor();
  }
  await page.keyboard.type("echo PHUX_SURVIVOR_OK");
  await page.keyboard.press("Enter");
  await waitForOutput(() => output.includes("\nPHUX_SURVIVOR_OK"));
  report(
    "closing panes leaves the surviving terminal usable",
    sockets === 1 && closed === 0,
  );
  await page.getByRole("button", { name: "Close live terminal" }).click();
  await page.waitForFunction(() =>
    document.activeElement?.hasAttribute("data-demo-launch"),
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

  const iframeContext = await browser.newContext();
  await iframeContext.route(`${BASE}/__embed-host`, (route) =>
    route.fulfill({
      contentType: "text/html",
      body: `<iframe src="${BASE}/embed" title="Hosted shell"></iframe>`,
    }),
  );
  await iframeContext.route(`${BASE}/auth/github?*`, (route) =>
    route.fulfill({
      contentType: "text/html",
      body: "<title>Controlled OAuth start</title>",
    }),
  );
  const iframeHost = await iframeContext.newPage();
  await iframeHost.goto(`${BASE}/__embed-host`);
  const shellFrame = iframeHost.frameLocator('iframe[title="Hosted shell"]');
  const popupOpened = iframeContext.waitForEvent("page");
  await shellFrame
    .getByRole("button", { name: "Continue with GitHub" })
    .click();
  const authPopup = await popupOpened;
  await authPopup.waitForURL(`${BASE}/auth/github?*`);
  report(
    "embedded GitHub sign-in escapes the iframe into a popup",
    iframeHost.frames().some((frame) => frame.url() === `${BASE}/embed`),
  );
  const popupClosed = authPopup.waitForEvent("close");
  await authPopup
    .goto(`${BASE}/embed?auth=error&auth_error=cancelled`)
    .catch((error) => {
      if (!authPopup.isClosed()) throw error;
    });
  await popupClosed;
  await shellFrame.getByRole("alert").waitFor();
  report(
    "failed popup sign-in returns an actionable error to the embedded shell",
    await shellFrame
      .getByRole("button", { name: "Continue with GitHub" })
      .isEnabled(),
  );
  const secondPopupOpened = iframeContext.waitForEvent("page");
  await shellFrame
    .getByRole("button", { name: "Continue with GitHub" })
    .click();
  const completedPopup = await secondPopupOpened;
  await completedPopup.waitForURL(`${BASE}/auth/github?*`);
  const completedPopupClosed = completedPopup.waitForEvent("close");
  await completedPopup.goto(`${BASE}/embed?auth=success`).catch((error) => {
    if (!completedPopup.isClosed()) throw error;
  });
  await completedPopupClosed;
  await shellFrame
    .getByRole("alert")
    .filter({ hasText: "could not read your session" })
    .waitFor();
  report(
    "embedded cookie restrictions direct the user to the standalone shell",
    await shellFrame
      .getByRole("link", { name: "Open standalone shell" })
      .isVisible(),
  );
  await iframeContext.close();

  const failedAuth = await browser.newContext();
  const failedAuthPage = await failedAuth.newPage();
  await failedAuthPage.goto(`${BASE}/?auth=error&auth_error=configuration`);
  await failedAuthPage.locator('dialog[open] [role="alert"]').waitFor();
  report(
    "same-tab sign-in errors reopen the dialog without leaking callback query",
    !new URL(failedAuthPage.url()).search,
  );
  await failedAuth.close();

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
