import assert from "node:assert/strict";
import { resolve } from "node:path";
import { createSignal, For, type Accessor, type Setter } from "solid-js";
import { createRoot, GpuixRenderer, type JSX } from "@gpuix/solid";
import { methods, type TreeNode } from "@gpuix/native/automation";
import type { GpuixRenderer as HostRenderer } from "../../native/generated";

declare global {
  var multiwindowHost: typeof import("../../native/generated");
}

interface FixtureWindow {
  name: string;
  renderer: HostRenderer;
  root: ReturnType<typeof createRoot>;
  events: unknown[];
  count: Accessor<number>;
  setCount: Setter<number>;
  value: Accessor<string>;
}

const host = globalThis.multiwindowHost;
assert.equal(GpuixRenderer, host.GpuixRenderer);
const errors: unknown[] = [];
const windows: FixtureWindow[] = [];

function makeWindow(name: string, width: number): FixtureWindow {
  let root: ReturnType<typeof createRoot>;
  const events: unknown[] = [];
  const renderer = new host.GpuixRenderer((error, event) => {
    if (error) errors.push(error);
    else {
      events.push(event);
      root?.dispatch(event);
    }
  });
  renderer.init({ title: name, width, height: 620, focus: false });
  root = createRoot(renderer, { onUncaughtError: (error) => errors.push(error) });
  const [count, setCount] = createSignal(0);
  const [value, setValue] = createSignal("");
  root.render((): JSX.Element => (
    <div
      testId="root"
      style={{
        width: "100%",
        height: "100%",
        padding: 16,
        flexDirection: "column",
        gap: 8,
        backgroundColor: "#182030",
        color: "#ffffff",
      }}
    >
      <text testId="counter">{`${name} count ${count()}`}</text>
      <div
        testId="button"
        onClick={() => setCount((n) => n + 1)}
        style={{ width: 160, height: 32, backgroundColor: "#305080" }}
      >
        <text>{`Increment ${name}`}</text>
      </div>
      <input
        testId="input"
        value={value()}
        onChange={(event) => setValue(event.value ?? "")}
        style={{ width: 220, height: 36 }}
      />
      <text
        testId="selection"
        highlight={{ query: "match", activeIndex: 1 }}
      >{`${name} match match`}</text>
      <div testId="scroll" style={{ width: 220, height: 80, overflowY: "scroll", flexShrink: 0 }}>
        <div style={{ height: 400, flexShrink: 0 }}>
          <text>{`${name} scroll`}</text>
        </div>
      </div>
      <virtual-list
        testId="list"
        estimatedItemHeight={24}
        style={{ width: 220, height: 110, flexShrink: 0 }}
      >
        <For each={Array.from({ length: 60 }, (_, i) => i)}>
          {(i): JSX.Element => <text style={{ height: 24 }}>{`${name} row ${i}`}</text>}
        </For>
      </virtual-list>
    </div>
  ));
  // The public Solid tag whitelist has no extension API. Exercise the installed
  // native probe through the existing low-level mutation API, under this root.
  renderer.applyBatch(
    JSON.stringify([
      ["createElement", 10000, "phux-host-probe"],
      ["setStyle", 10000, { width: 200, height: 48 }],
      ["setCustomProp", 10000, "label", `${name} native`],
      ["appendChild", 1, 10000],
    ]),
  );
  const result = { name, renderer, root, events, count, setCount, value };
  windows.push(result);
  return result;
}

function nodes(window: FixtureWindow) {
  const raw: unknown = JSON.parse(window.renderer.getAutomationTree());
  const { tree } = methods.getTree.result.parse({ tree: raw });
  assert.ok(tree, `${window.name}: automation tree`);
  const result = new Map<string, TreeNode>();
  function visit(node: TreeNode) {
    if (node.testId) result.set(node.testId, node);
    for (const child of node.children ?? []) visit(child);
  }
  visit(tree);
  return result;
}

async function pump(renderer: HostRenderer, frames = 12) {
  for (let i = 0; i < frames; i++) {
    assert.equal(renderer.tick(), true, "shared platform must stay alive");
    await Bun.sleep(12);
  }
  assert.deepEqual(errors, []);
}

function node(window: FixtureWindow, testId: string) {
  const result = nodes(window).get(testId);
  assert.ok(result, `${window.name}:${testId} automation node`);
  return result;
}

function paintedBounds(window: FixtureWindow, testId: string) {
  const bounds = window.renderer.getElementBounds(id(window, testId));
  assert.ok(bounds, `${window.name}:${testId} painted bounds`);
  return bounds;
}

function click(window: FixtureWindow, testId: string) {
  const bounds = paintedBounds(window, testId);
  window.renderer.simulateClick(bounds.x + 8, bounds.y + 8);
}

function id(window: FixtureWindow, testId: string) {
  return node(window, testId).id;
}

function listScrollIndex(window: FixtureWindow) {
  const position = window.renderer.getListScrollTop(id(window, "list"));
  assert.ok(position, `${window.name}:list scroll position`);
  const index = position[0];
  assert.ok(index !== undefined, `${window.name}:list scroll index`);
  return index;
}

function selectText(window: FixtureWindow) {
  const bounds = paintedBounds(window, "selection");
  window.renderer.simulateMouseDown(bounds.x + 1, bounds.y + 10);
  window.renderer.simulateMouseMove(bounds.x + 180, bounds.y + 10, 0);
  window.renderer.simulateMouseUp(bounds.x + 180, bounds.y + 10);
}

function assertPaint(window: FixtureWindow, counter: number) {
  const text = window.renderer.getPaintedText();
  assert.ok(text.includes(`${window.name} count ${counter}`), `${window.name} reactive paint`);
  assert.ok(text.includes(`${window.name} native`), `${window.name} native extension paint`);
}

async function verifyInput(a: FixtureWindow, b: FixtureWindow) {
  a.renderer.focusElement(id(a, "input"));
  b.renderer.focusElement(id(b, "input"));
  await pump(a.renderer);
  assert.equal(a.renderer.getFocusedElementId(), id(a, "input"));
  assert.equal(b.renderer.getFocusedElementId(), id(b, "input"));
  a.renderer.simulateKeystrokes("a l p h a");
  b.renderer.simulateKeystrokes("b e t a");
  await pump(b.renderer);
  assert.equal(a.value(), "alpha");
  assert.equal(b.value(), "beta");
  a.renderer.blur();
  assert.equal(a.renderer.getFocusedElementId(), null);
  assert.equal(b.renderer.getFocusedElementId(), id(b, "input"));
}

async function verifyScroll(a: FixtureWindow, b: FixtureWindow) {
  const aScroll = id(a, "scroll"),
    bScroll = id(b, "scroll");
  const bounds = paintedBounds(a, "scroll");
  a.renderer.scrollTo(aScroll, 0, -110);
  b.renderer.scrollTo(bScroll, 0, -45);
  a.renderer.scrollToItem(id(a, "list"), 15, 0);
  b.renderer.scrollToItem(id(b, "list"), 30, 0);
  await pump(b.renderer);
  assert.deepEqual(a.renderer.getScrollOffset(aScroll), [0, -110]);
  assert.deepEqual(b.renderer.getScrollOffset(bScroll), [0, -45]);
  assert.equal(listScrollIndex(a), 15);
  assert.equal(listScrollIndex(b), 30);
  a.renderer.simulateMouseMove(bounds.x + 10, bounds.y + 10);
  a.renderer.simulateScrollWheel(bounds.x + 10, bounds.y + 10, 0, -20);
  await pump(a.renderer);
  assert.notDeepEqual(a.renderer.getScrollOffset(aScroll), [0, -110]);
  assert.deepEqual(b.renderer.getScrollOffset(bScroll), [0, -45]);
}

async function verifySelection(a: FixtureWindow, b: FixtureWindow) {
  selectText(a);
  await pump(b.renderer);
  const selectedA = a.renderer.getSelectedText();
  assert.ok(selectedA?.startsWith("alpha"), `alpha selected: ${selectedA}`);
  assert.equal(b.renderer.getSelectedText(), null);
  selectText(b);
  await pump(a.renderer);
  assert.ok(b.renderer.getSelectedText()?.startsWith("beta"));
  assert.equal(a.renderer.getSelectedText(), selectedA);
  const highlights = a.renderer.getPaintedHighlights();
  assert.equal(highlights.length, 2);
  assert.deepEqual(
    highlights.map((h) => h.active),
    [false, true],
  );
  assert.ok(highlights.every((h) => h.text.startsWith("alpha")));
  assert.deepEqual(
    b.renderer.getPaintedHighlights().map((h) => h.active),
    [false, true],
  );
  assert.ok(b.renderer.getPaintedHighlights().every((h) => h.text.startsWith("beta")));
  a.renderer.clearSelection();
  await pump(b.renderer);
  assert.equal(a.renderer.getSelectedText(), null);
  assert.ok(b.renderer.getSelectedText()?.startsWith("beta"));
}

async function verifyClose(a: FixtureWindow, b: FixtureWindow) {
  const oldId = a.renderer.getWindowId();
  const staleButton = id(a, "button");
  // Queue native work, then close before napi's non-blocking callback delivery.
  click(a, "button");
  a.renderer.closeWindow();
  await pump(b.renderer);
  assert.equal(a.count(), 7, "queued callback fenced after close");
  assert.equal(a.root.dispatch({ elementId: staleButton, eventType: "click" }), false);
  assert.equal(a.renderer.isWindowOpen(), false);
  assert.equal(b.renderer.isWindowOpen(), true);
  assert.throws(() => a.renderer.getElementBounds(staleButton), /closed/);
  assert.throws(() => a.renderer.applyBatch("[]"), /closed/);
  assert.deepEqual(a.renderer.getPaintedText(), []);
  a.root.unmount();
  assert.equal(host.desktopHostProbeCounts().destroyed, 1);
  assertPaint(b, 9);
  assert.equal(listScrollIndex(b), 30);
  assert.deepEqual(b.renderer.getScrollOffset(id(b, "scroll")), [0, -45]);
  assert.equal(b.value(), "beta");
  assert.ok(b.renderer.getSelectedText()?.startsWith("beta"));
  const c = makeWindow("gamma", 420);
  await pump(b.renderer);
  assert.notEqual(c.renderer.getWindowId(), oldId);
  assert.equal(id(c, "button"), staleButton);
  click(c, "button");
  await pump(c.renderer);
  assertPaint(c, 1);
  assertPaint(b, 9);
  // The real Window menu's registered close action is dispatched by cmd-w.
  // Activating beta must not make gamma's API close beta instead.
  b.renderer.activateWindow();
  c.renderer.focusElement(id(c, "input"));
  c.renderer.simulateKeystrokes("cmd-w");
  await pump(b.renderer);
  assert.equal(c.renderer.isWindowOpen(), false, "menu Close targets its own window");
  assert.equal(b.renderer.isWindowOpen(), true);
  c.root.unmount();
  click(b, "button");
  await pump(b.renderer);
  assertPaint(b, 10);
  assert.equal(host.desktopHostProbeCounts().destroyed, 2);
  b.renderer.closeWindow();
  b.root.unmount();
  for (let i = 0; i < 30 && b.renderer.tick(); i++) await Bun.sleep(12);
  assert.equal(b.renderer.tick(), false, "last close returns control to the JS host");
  assert.equal(host.desktopHostProbeCounts().destroyed, 3);
  assert.equal(host.desktopHostProbeCounts().dropped, 3);
  const d = makeWindow("delta", 400);
  await pump(d.renderer);
  assertPaint(d, 0);
  assert.notEqual(d.renderer.getWindowId(), oldId);
  assert.throws(() => a.renderer.focusElement(staleButton), /closed/);
  d.renderer.closeWindow();
  d.root.unmount();
  assert.equal(d.renderer.tick(), false);
  assert.equal(host.desktopHostProbeCounts().destroyed, 4);
  assert.equal(host.desktopHostProbeCounts().dropped, 4);
}

try {
  const a = makeWindow("alpha", 380);
  const b = makeWindow("beta", 460);
  a.renderer.focusElement(id(a, "input"));
  b.renderer.focusElement(id(b, "input"));
  a.renderer.scrollToItem(id(a, "list"), 3, 0);
  b.renderer.scrollToItem(id(b, "list"), 8, 0);
  await pump(a.renderer);
  assert.equal(a.renderer.getFocusedElementId(), id(a, "input"), "pending alpha focus");
  assert.equal(b.renderer.getFocusedElementId(), id(b, "input"), "pending beta focus");
  assert.equal(listScrollIndex(a), 3, "pending alpha scroll");
  assert.equal(listScrollIndex(b), 8, "pending beta scroll");
  assert.notEqual(a.renderer.getWindowId(), b.renderer.getWindowId());
  assert.equal(a.renderer.getWindowTitle(), "alpha");
  assert.equal(b.renderer.getWindowTitle(), "beta");
  a.renderer.setWindowTitle("alpha updated");
  await pump(b.renderer);
  assert.equal(a.renderer.getWindowTitle(), "alpha updated");
  assert.equal(b.renderer.getWindowTitle(), "beta");
  const aBounds = node(a, "root").bounds;
  const bBounds = node(b, "root").bounds;
  assert.ok(aBounds, "alpha:root automation bounds");
  assert.ok(bBounds, "beta:root automation bounds");
  assert.notEqual(aBounds.width, bBounds.width);
  assert.equal(id(a, "button"), id(b, "button"), "deliberately overlapping local IDs");
  click(a, "button");
  await pump(b.renderer);
  assert.equal(a.count(), 1);
  assert.equal(b.count(), 0);
  click(b, "button");
  await pump(a.renderer);
  assert.equal(b.count(), 1);
  a.root.flushSync(() => a.setCount(7));
  b.root.flushSync(() => b.setCount(9));
  await pump(a.renderer);
  assertPaint(a, 7);
  assertPaint(b, 9);
  await verifyInput(a, b);
  await verifyScroll(a, b);
  await verifySelection(a, b);
  a.renderer.captureScreenshot(
    resolve(process.env.MULTIWINDOW_ARTIFACTS!, "multiwindow-alpha.png"),
  );
  b.renderer.captureScreenshot(resolve(process.env.MULTIWINDOW_ARTIFACTS!, "multiwindow-beta.png"));
  await verifyClose(a, b);
  console.log("MULTIWINDOW_PASS", host.desktopHostProbeCounts());
} finally {
  for (const window of windows) {
    window.root.unmount();
    if (window.renderer.isWindowOpen()) window.renderer.closeWindow();
  }
}
process.exit(0);
