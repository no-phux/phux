import assert from "node:assert/strict";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { loadDesktopHost } from "../../native/loader.mjs";

const host = loadDesktopHost(process.argv[2]);
const client = new host.DesktopClient();
const renderer = new host.TestGpuixRenderer(1200, 340);
const artifact = resolve(import.meta.dirname, "../../.cache/terminal-painter");
mkdirSync(artifact, { recursive: true });
rmSync(resolve(artifact, "pixel-receipt.json"), { force: true });
let closed = false;
let revision = 0;
const events = [];
let mounted = false;
let views = [];
const batch = (ops) => {
  renderer.applyBatch(JSON.stringify(ops));
  renderer.flush();
};
function repaint() {
  if (mounted) batch([2, 3].map((id) => ["setCustomProp", id, "paintRevision", ++revision]));
}
function report(id) {
  return host.terminalFixturePaints().find((report) => report.id === id);
}
function text(id) {
  return (
    report(id)
      ?.glyphs.map((glyph) => glyph.text)
      .join("") ?? ""
  );
}
async function until(predicate, name) {
  const deadline = Date.now() + 15000;
  while (!predicate()) {
    if (Date.now() >= deadline) {
      console.error(
        "publication generations",
        views.map((view) => host.terminalFixtureGeneration(client.handle, view)),
      );
      writeFileSync(
        resolve(artifact, "failure.json"),
        JSON.stringify(host.terminalFixturePaints(), null, 2),
      );
      renderer.captureScreenshot(resolve(artifact, "failure.png"));
    }
    assert.ok(
      Date.now() < deadline,
      `timeout: ${name}; ${JSON.stringify(host.terminalFixturePaints()).slice(0, 500)}`,
    );
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}
async function send(command) {
  const id = client.applyPaste(terminal, command);
  await until(() => events.some((event) => event.deliveryId === id), `delivery ${command}`);
  assert.equal(events.find((event) => event.deliveryId === id).outcome, "Delivered");
}
async function rasterFixtures() {
  await send("PIXELS");
  await until(() => text(2).includes("PIXELREADY"), "raster fixture paint");
  const geometry = report(2);
  const capture = (name) => renderer.captureScreenshot(resolve(artifact, `pixels-${name}.png`));
  writeFileSync(
    resolve(artifact, "pixel-geometry.json"),
    JSON.stringify({
      cellWidth: geometry.cellWidth,
      cellHeight: geometry.cellHeight,
      scale: geometry.scale,
    }),
  );
  capture("block");
  batch([["setCustomProp", 2, "focused", false]]);
  capture("hollow");
  batch([
    ["setCustomProp", 2, "focused", true],
    ["setCustomProp", 2, "cursorVisible", false],
  ]);
  capture("hidden");
  batch([["setCustomProp", 2, "cursorVisible", true]]);
  for (const [command, name] of [
    ["CURSORBAR", "bar"],
    ["CURSORUNDER", "underline"],
  ]) {
    const generation = report(2).generation;
    await send(command);
    await until(() => report(2).generation !== generation, `${name} cursor paint`);
    capture(name);
  }
  const clipWidth = geometry.cellWidth * 5;
  const clipHeight = geometry.cellHeight * 6.5;
  batch([
    ["setStyle", 2, { width: clipWidth, height: clipHeight }],
    ["setStyle", 3, { width: 0, height: 0 }],
  ]);
  capture("clipped");
  batch([
    ["setStyle", 2, { width: 600, height: 320 }],
    ["setStyle", 3, { width: 600, height: 320 }],
  ]);
  await send("DRAW");
  await until(
    () => text(2).startsWith("GPUI NATIVE TERMINAL") && text(2).includes("READY"),
    "restore dense fixture",
  );
}
let terminal;
try {
  client.connect({ socketPath: process.argv[3], cols: 60, rows: 16 }, (handle) => {
    if (closed) return;
    assert.equal(handle, client.handle);
    events.push(...client.takeEvents());
    repaint();
  });
  await until(
    () => client.topology()?.sessions.some((session) => session.name === "terminal-painter"),
    "topology",
  );
  client.attachSession("terminal-painter");
  await until(() => client.status() === "Attached", "attach");
  terminal = client
    .topology()
    .panes.find((pane) => pane.sessionName === "terminal-painter").terminalId;
  await until(() => client.inputReadiness(terminal).ready, "input ready");
  const left = host.terminalFixtureCreateView(client.handle, terminal);
  const right = host.terminalFixtureCreateView(client.handle, terminal);
  views = [left, right];
  assert.notEqual(left, right);
  const ops = [
    ["createElement", 1, "div"],
    [
      "setStyle",
      1,
      {
        width: 1200,
        height: 340,
        display: "flex",
        flexDirection: "row",
        backgroundColor: "#fa00fa",
      },
    ],
    ["setRoot", 1],
  ];
  for (const [id, view] of [
    [2, left],
    [3, right],
  ])
    ops.push(
      ["createElement", id, "phux-terminal"],
      ["setStyle", id, { width: 600, height: 320 }],
      ["setCustomProp", id, "clientHandle", client.handle],
      ["setCustomProp", id, "terminalId", terminal],
      ["setCustomProp", id, "viewId", view],
      ["appendChild", 1, id],
    );
  batch(ops);
  mounted = true;
  await send("DRAW");
  await until(
    () => text(2).includes("READY") && text(3).includes("READY"),
    "paint actual PTY output",
  );
  assert.deepEqual(
    renderer.getPaintedText(),
    [],
    "terminal does not synthesize GPUIX variable-width text rows",
  );
  const initial = report(2);
  assert.equal(initial.error, null);
  assert.equal(initial.cols, 60);
  assert.equal(initial.rows, 16);
  const unicode = initial.glyphs.filter((glyph) => glyph.row === 1);
  assert.deepEqual(
    unicode.map(({ text, col }) => [text, col]),
    [
      ["A", 0],
      ["界", 1],
      ["é", 3],
      ["🙂", 4],
      ["Z", 6],
    ],
  );
  for (const glyph of unicode) {
    assert.equal(glyph.paintX, unicode[0].x + glyph.col * initial.cellWidth);
    assert.ok(
      Math.abs(glyph.baseline - glyph.y - initial.baselineOffset) < 0.001,
      "fallback and combining glyphs share the fixed row baseline",
    );
  }
  assert.equal(unicode[1].width, initial.cellWidth * 2);
  assert.equal(unicode[2].width, initial.cellWidth);
  assert.equal(unicode[3].width, initial.cellWidth * 2);
  assert.ok(initial.glyphs.some((glyph) => glyph.hyperlink === "https://example.test/native"));
  assert.ok(!text(2).includes("SECRET"), "SGR invisible suppresses glyph painting");
  const red = initial.glyphs.find((glyph) => glyph.row === 2 && glyph.col === 0);
  const inverse = initial.glyphs.find((glyph) => glyph.row === 2 && glyph.col === 3);
  assert.ok(Math.abs(red.foreground[0] - 240 / 255) < 0.001);
  assert.ok(Math.abs(inverse.foreground[2] - 160 / 255) < 0.001);
  renderer.captureScreenshot(resolve(artifact, "fidelity.png"));
  writeFileSync(
    resolve(artifact, "initial.json"),
    JSON.stringify(host.terminalFixturePaints(), null, 2),
  );

  const timings = [];
  for (let frame = 0; frame < 30; frame++) {
    const started = performance.now();
    repaint();
    timings.push({
      flushMillis: performance.now() - started,
      prepareMicros: report(2).prepareMicros,
      paintMicros: report(2).paintMicros,
    });
  }
  writeFileSync(resolve(artifact, "timings.json"), JSON.stringify(timings, null, 2));
  await rasterFixtures();

  host.terminalFixtureScroll(client.handle, left, -8);
  host.terminalFixtureSelect(client.handle, right, "RGB");
  repaint(); // local publication must paint even without a socket event
  assert.notEqual(report(2).scrollOffset, report(3).scrollOffset);
  assert.ok(report(3).glyphs.some((glyph) => glyph.flags & (1 << 8)));
  assert.ok(!report(2).glyphs.some((glyph) => glyph.flags & (1 << 8)));
  renderer.captureScreenshot(resolve(artifact, "two-views.png"));

  const previousWidth = report(3).cellWidth;
  batch([
    ["setCustomProp", 3, "font", { family: "Menlo", size: 18, lineHeight: 1.4 }],
    ["setCustomProp", 3, "theme", { background: "#152030", foreground: "#b0d0f0" }],
    ["setCustomProp", 3, "focused", false],
  ]);
  assert.ok(report(3).cellWidth > previousWidth);
  assert.equal(report(3).cols, 60, "surface geometry cannot resize shared PTY");
  assert.ok(report(3).glyphs.every((glyph) => glyph.x < 1200 && glyph.y < 320));
  renderer.captureScreenshot(resolve(artifact, "font-clipping.png"));
  batch([["setCustomProp", 3, "font", null]]);
  await send("UPDATE");
  await until(() => text(3).includes("UPDATED FRAME"), "new output in live view");
  // Keep the runtime regression reproducible. With selection retained, the
  // current runtime stops publishing this view on DECSET 1049 (phux-d4x9.17).
  if (process.env.PHUX_TERMINAL_ALT_WITH_SELECTION !== "1") {
    host.terminalFixtureClearSelection(client.handle, right);
    repaint();
  }
  await send("ALT");
  await until(() => text(3).includes("ALTERNATE SCREEN"), "alternate screen");
  await send("MAIN");
  await until(() => text(3).includes("UPDATED FRAME"), "main screen restored");

  host.terminalFixtureDestroyView(client.handle, left);
  repaint();
  assert.equal(report(2).glyphs.length, 0, "removed slot clears old native text");
  assert.match(report(2).error, /no publication/);
  batch([["destroyElement", 2]]);
  assert.equal(report(2), undefined, "observation teardown follows view teardown");
  const replacement = host.terminalFixtureCreateView(client.handle, terminal);
  assert.notEqual(replacement, left);
  batch([
    ["createElement", 2, "phux-terminal"],
    ["setStyle", 2, { width: 600, height: 320 }],
    ["setCustomProp", 2, "clientHandle", client.handle],
    ["setCustomProp", 2, "terminalId", terminal],
    ["setCustomProp", 2, "viewId", replacement],
    ["appendChild", 1, 2],
  ]);
  assert.ok(text(2).includes("UPDATED FRAME"), "reused host ID receives fresh view state");
  batch([["setCustomProp", 2, "viewId", Number(replacement)]]);
  assert.equal(report(2).glyphs.length, 0, "numeric view handles are rejected, not rounded");
  batch([["destroyElement", 2]]);
  host.terminalFixtureDestroyView(client.handle, replacement);
  mounted = false;
  closed = true;
  client.close();
  batch([["setCustomProp", 3, "paintRevision", ++revision]]);
  assert.equal(report(3).glyphs.length, 0);
  assert.match(report(3).error, /StaleHandle/);
  batch([["destroyElement", 3]]);
  assert.deepEqual(host.terminalFixturePaints(), []);
  console.log(
    JSON.stringify({
      result: "pass",
      artifact,
      scale: initial.scale,
      glyphs: initial.glyphs.length,
      prepareMicros: initial.prepareMicros,
      paintMicros: initial.paintMicros,
    }),
  );
} finally {
  if (!closed) {
    closed = true;
    client.close();
  }
}
