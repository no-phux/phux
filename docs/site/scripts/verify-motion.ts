#!/usr/bin/env bun
/** Browser acceptance against dist: no live phux server or production socket. */
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { resolve, extname } from "node:path";
import { chromium, type Page } from "playwright-core";

const root = resolve(import.meta.dir, "../dist");
const artifacts =
  process.env.PHUX_MOTION_ARTIFACTS ??
  resolve(import.meta.dir, "../.motion-check");
const types: Record<string, string> = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
  ".png": "image/png",
  ".mp4": "video/mp4",
};
const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  async fetch(request) {
    const pathname = new URL(request.url).pathname;
    let path = resolve(root, `.${pathname}`);
    if (!path.startsWith(`${root}/`) && path !== root)
      return new Response(null, { status: 403 });
    if (pathname === "/") path = resolve(root, "index.html");
    if (!extname(path)) path += ".html";
    const file = Bun.file(path);
    if (!(await file.exists())) return new Response(null, { status: 404 });
    return new Response(file, {
      headers: { "content-type": types[extname(path)] ?? file.type },
    });
  },
});
const browser = await chromium.launch({ channel: "chrome", headless: true });
const base = `http://127.0.0.1:${server.port}`;
const errors: string[] = [];

async function seek(page: Page, value: number) {
  await page
    .locator("#how-it-works input[type=range]")
    .evaluate((input, time) => {
      const setter = Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!;
      setter.call(input, String(time));
      input.dispatchEvent(new Event("input", { bubbles: true }));
      input.dispatchEvent(new Event("change", { bubbles: true }));
    }, value);
}

async function timeline(page: Page) {
  return Number(
    await page.locator("#how-it-works input[type=range]").inputValue(),
  );
}

try {
  await mkdir(artifacts, { recursive: true });
  const page = await browser.newPage({
    viewport: { width: 1280, height: 1000 },
  });
  page.on("pageerror", (error) => errors.push(error.message));
  const requests: string[] = [];
  page.on("request", (request) => requests.push(request.url()));
  await page.goto(base);
  const stage = page.locator("#how-it-works");
  await stage.scrollIntoViewIfNeeded();
  await page.waitForFunction(
    () =>
      !document.querySelector<HTMLButtonElement>(".playback-controls button")
        ?.disabled,
  );
  assert.equal(await timeline(page), 0, "no autoplay");
  await page.waitForTimeout(200);
  assert.equal(await timeline(page), 0, "idle clock remains still");
  await stage.screenshot({ path: `${artifacts}/desktop.png` });

  for (const label of [
    "Leave. Come back.",
    "Human + agents",
    "Across machines",
    "Under the hood",
  ]) {
    await stage.getByRole("button", { name: label }).click();
    assert.equal(await timeline(page), 0, "scenario selection resets clock");
    for (let index = 1; index <= 4; index++) {
      await stage
        .getByRole("button", { name: new RegExp(`^Step ${index}:`) })
        .click();
      assert.equal(await timeline(page), (index - 1) * 2.5);
      assert.equal(
        await stage.locator(".step-counter").innerText(),
        `${index} / 4`,
      );
    }
    await stage.getByRole("button", { name: "Protocol detail off" }).click();
    assert.ok((await stage.locator(".beat-copy p").innerText()).length > 20);
    await stage.getByRole("button", { name: "Protocol detail on" }).click();
  }
  await stage.getByLabel("Client", { exact: true }).selectOption("iPhone");
  assert.ok(
    (await stage.locator(".motion-diagram.wide").textContent())?.includes(
      "iPhone",
    ),
  );
  await seek(page, 0.6);
  const beforeX = await stage
    .locator(".wide .signal-packet circle")
    .last()
    .getAttribute("cx");
  await seek(page, 1.2);
  const afterX = await stage
    .locator(".wide .signal-packet circle")
    .last()
    .getAttribute("cx");
  assert.notEqual(beforeX, afterX, "scrubbing moves signal along its route");
  await stage.screenshot({ path: `${artifacts}/signal-in-flight.png` });
  await seek(page, 10);
  assert.equal(await stage.locator(".step-counter").innerText(), "4 / 4");
  await stage.getByRole("button", { name: "Play scenario" }).click();
  await page.waitForTimeout(600);
  assert.ok(
    (await timeline(page)) < 2,
    "completed scenario replays from start",
  );
  await stage.getByRole("button", { name: "Pause", exact: true }).click();
  const paused = await timeline(page);
  await page.waitForTimeout(200);
  assert.equal(await timeline(page), paused);

  await seek(page, 9.6);
  await stage.getByRole("button", { name: "Play scenario" }).click();
  await page.waitForTimeout(650);
  assert.equal(await timeline(page), 10, "playback stops at the final frame");
  assert.ok(
    await stage.getByRole("button", { name: "Play scenario" }).isVisible(),
  );
  const finalX = await stage
    .locator(".wide .signal-packet circle")
    .last()
    .getAttribute("cx");
  assert.equal(finalX, "224", "final reply stays at the client port");
  await seek(page, 0);

  await stage.getByRole("button", { name: "Play scenario" }).click();
  await page.waitForTimeout(200);
  await page.setViewportSize({ width: 1280, height: 600 });
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.waitForTimeout(250);
  const offscreen = await timeline(page);
  await page.waitForTimeout(250);
  assert.equal(await timeline(page), offscreen, "offscreen clock sleeps");
  await stage.scrollIntoViewIfNeeded();
  await page.waitForTimeout(200);
  assert.ok((await timeline(page)) > offscreen, "visible clock resumes");
  await stage.locator(".explainer-story").evaluate((story) => {
    story.scrollIntoView({ block: "start" });
    window.scrollBy(0, 20);
  });
  await page.waitForTimeout(150);
  const readingStory = await timeline(page);
  await page.waitForTimeout(200);
  assert.equal(
    await timeline(page),
    readingStory,
    "clock sleeps while only the story is visible",
  );
  await stage.locator(".explainer-stage").scrollIntoViewIfNeeded();
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.waitForTimeout(100);
  assert.ok(
    await stage.getByRole("button", { name: "Play scenario" }).isDisabled(),
  );
  const reducedTime = await timeline(page);
  await page.waitForTimeout(150);
  assert.equal(await timeline(page), reducedTime);
  await stage.getByRole("button", { name: /^Step 3:/ }).click();
  assert.equal(await timeline(page), 5);
  assert.equal(await stage.locator(".signal-packet").count(), 0);

  for (const width of [320, 768, 1280]) {
    await page.setViewportSize({ width, height: 1000 });
    await stage.scrollIntoViewIfNeeded();
    assert.ok(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
      `no overflow at ${width}px`,
    );
    await stage.screenshot({ path: `${artifacts}/still-${width}.png` });
  }
  await page.setViewportSize({ width: 320, height: 1000 });
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await stage.getByRole("button", { name: "Across machines" }).click();
  await stage.screenshot({ path: `${artifacts}/mobile-federation.png` });
  await stage.getByRole("button", { name: /^Step 2:/ }).focus();
  await page.keyboard.press("Enter");
  assert.equal(await timeline(page), 2.5, "keyboard step navigation");
  assert.equal(
    await stage
      .locator(".compact .active-wire")
      .evaluate((wire) => getComputedStyle(wire).fill),
    "none",
    "curved routes never fill their chord",
  );
  await stage.getByRole("slider", { name: "Timeline" }).focus();
  await page.keyboard.press("ArrowRight");
  assert.ok((await timeline(page)) > 2.5, "keyboard scrubbing");
  assert.ok(
    !requests.some((url) => /\.wasm|\/auth\/session|\/healthz/.test(url)),
    "explainers don't allocate live sessions or load WASM",
  );
  assert.deepEqual(errors, [], "no browser errors");
  assert.ok(
    !requests.some((url) => url.endsWith("phux-architecture.mp4")),
    "film isn't fetched before user plays it",
  );
  await stage.locator(".architecture-film summary").click();
  await stage.locator("video").evaluate(async (video) => {
    await (video as HTMLVideoElement).play();
  });
  assert.equal(
    await stage.locator("video").evaluate((video) => (video as HTMLVideoElement).videoWidth),
    1920,
  );
  assert.equal(
    await stage.locator("video").evaluate((video) => (video as HTMLVideoElement).duration),
    16,
  );
  await stage.locator(".architecture-film summary").click();
  await page.waitForFunction(
    () =>
      document.querySelector<HTMLVideoElement>(".architecture-film video")
        ?.paused,
  );
  assert.ok(
    await stage.locator("video").evaluate((video) => (video as HTMLVideoElement).paused),
    "closing film pauses playback",
  );
  await page.close();

  const context = await browser.newContext({
    javaScriptEnabled: false,
    viewport: { width: 320, height: 1000 },
  });
  const staticPage = await context.newPage();
  await staticPage.goto(base);
  assert.ok(
    await staticPage.locator("#how-it-works h3").isVisible(),
    "server rendered story survives without JS",
  );
  assert.ok(await staticPage.locator("#how-it-works noscript").isVisible());
  assert.equal(
    await staticPage
      .locator("#how-it-works .explainer-story a")
      .getAttribute("href"),
    "https://docs.phux.sh/concepts",
  );
  await context.close();
  console.log(
    `PASS motion: four scenarios, clients/detail, deterministic scrub, play/pause/replay, offscreen sleep, reduced motion, keyboard, 320/768/1280px, no-JS. Screenshots: ${artifacts}`,
  );
} finally {
  await browser.close();
  server.stop(true);
}
