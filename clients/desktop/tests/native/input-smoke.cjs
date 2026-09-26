// GPU dispatch + native IME adapter + shared registry + isolated real PTY.
// The only text snapshot export belongs to the temporary test fixture.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");
const host = require(process.argv[2]);
const socketPath = process.argv[3];
const temp = fs.mkdtempSync(path.join(os.tmpdir(), "phux-input-"));
const capture = path.join(temp, "bytes");
const script = path.join(temp, "capture.py");
fs.writeFileSync(
  script,
  `import os,tty,termios\nold=termios.tcgetattr(0)\ntry:\n tty.setraw(0)\n os.write(1,b'line\\r\\n'*40+b'CAPTURE_READY')\n with open(${JSON.stringify(capture)},'wb',buffering=0) as f:\n  while True:\n   b=os.read(0,1)\n   if b==b'\\x04': break\n   if b==b'\\x05': os.write(1,b'\\x1b[?1000h\\x1b[?1006h')\n   if b==b'\\x06': os.write(1,b'\\x1b[>11uKITTY_READY')\n   f.write(b)\nfinally:\n termios.tcsetattr(0,termios.TCSANOW,old)\n`,
);
const bytes = () => (fs.existsSync(capture) ? fs.readFileSync(capture, "utf8") : "");
const client = new host.DesktopClient();
const events = [];
let closed = false;
host.initializeDesktopHost();
const renderer = new host.TestGpuixRenderer(800, 480);
const batch = (ops) => {
  renderer.applyBatch(JSON.stringify(ops));
  renderer.flush();
};
const snapshot = () => JSON.parse(host.inputFixtureSnapshot(2));
let sequence = 0;
const command = (kind, text = "", extra = {}) =>
  batch([["setCustomProp", 2, "command", { kind, text, sequence: ++sequence, ...extra }]]);
async function until(predicate, message) {
  const deadline = Date.now() + 15000;
  while (!predicate()) {
    assert.ok(
      Date.now() < deadline,
      `${message}; snapshot=${host.inputFixtureSnapshot(2)}; bytes=${JSON.stringify(bytes())}; events=${JSON.stringify(events.slice(-8))}; runtimeError=${client.lastError()}`,
    );
    await new Promise((resolve) => setTimeout(resolve, 10));
    renderer.flush();
  }
}
async function main() {
  client.connect({ socketPath, cols: 80, rows: 24 }, () => {
    if (!closed) events.push(...client.takeEvents());
  });
  await until(() => client.topology()?.sessions.some((s) => s.name === "input-smoke"), "topology");
  client.attachSession("input-smoke");
  await until(() => client.status() === "Attached", "attach");
  const pane = client.topology().panes.find((p) => p.sessionName === "input-smoke");
  await until(
    () => events.some((e) => e.kind === "AttachAnswered" && e.terminalId === pane.terminalId),
    "initial terminal subscription confirmed",
  );
  await until(() => client.inputReadiness(pane.terminalId).ready, "readiness");
  const terminal = Number(pane.terminalId.slice("local:".length));
  const view = host.inputFixtureCreateView(client.handle, terminal);
  const sibling = host.inputFixtureCreateView(client.handle, terminal);
  const viewState = (value) => JSON.parse(host.inputFixtureViewState(client.handle, value));
  batch([
    ["createElement", 1, "div"],
    ["setStyle", 1, { width: 800, height: 480 }],
    ["setRoot", 1],
    ["createElement", 2, "phux-input-fixture"],
    ["setStyle", 2, { width: 800, height: 480 }],
    ["setCustomProp", 2, "handle", client.handle],
    ["setCustomProp", 2, "view", view],
    ["setCustomProp", 2, "terminal", terminal],
    ["appendChild", 1, 2],
  ]);
  renderer.simulateClick(15, 15);
  renderer.flush();
  assert.equal(snapshot().focused, true);
  command("commit", `python3 ${script}\n`);
  await until(() => fs.existsSync(capture), "PTY byte capture");
  renderer.simulateKeystrokes("a");
  await until(() => bytes() === "a", "printable committed exactly once");
  renderer.simulateKeyDown("b->b", true);
  command("commit", "b");
  renderer.simulateKeyUp("b");
  await until(() => bytes() === "ab", "repeat commit exactly once and release");
  command("mark", "に");
  assert.equal(snapshot().preedit, "に");
  command("mark", "日本");
  assert.equal(snapshot().preedit, "日本");
  assert.equal(bytes(), "ab", "preedit is local");
  command("commit", "日本");
  await until(() => bytes() === "ab日本", "composition commit once");
  command("unmark");
  command("mark", "👩‍👩‍👧‍👦");
  assert.deepEqual(snapshot().selected, [0, 11], "UTF16 selection rounds to complete grapheme");
  command("unmark");
  assert.equal(snapshot().preedit, "");
  assert.equal(bytes(), "ab日本", "unmark does not emit canceled composition");
  command("mark", "cancel-on-blur");
  command("active", "", { enabled: false });
  command("commit", "inactive");
  assert.match(snapshot().error, /WrongFocus/);
  assert.equal(snapshot().preedit, "");
  assert.equal(bytes(), "ab日本");
  command("active", "", { enabled: true });
  command("commit", "\x05");
  await until(() => viewState(view).mouse === "Normal", "terminal mouse mode publication");
  renderer.simulateClick(25, 25);
  await until(
    () => /\x1b\[<0;\d+;\d+M\x1b\[<0;\d+;\d+m$/.test(bytes()),
    "mouse protocol press/release",
  );
  const beforeSelection = bytes();
  renderer.simulateMouseDown(5, 5, 0, "shift");
  renderer.flush();
  renderer.simulateMouseMove(100, 5, 0, "shift");
  renderer.flush();
  renderer.simulateMouseUp(100, 5, 0, "shift");
  renderer.flush();
  assert.ok(viewState(view).selection, "Shift selects locally while application reports mouse");
  assert.equal(viewState(sibling).selection, null, "selection is view-local");
  assert.equal(bytes(), beforeSelection, "Shift selection is never sent to application");
  const siblingOffset = viewState(sibling).offset;
  const ownOffset = viewState(view).offset;
  renderer.simulateScrollWheel(25, 25, 0, 20, "shift");
  renderer.flush();
  assert.ok(viewState(view).offset < ownOffset, "wheel scrolls the input view");
  assert.equal(viewState(sibling).offset, siblingOffset, "sibling viewport is preserved");
  renderer.simulateScrollWheel(25, 25, 0, -20, "shift");
  renderer.flush();
  command("commit", "\x06");
  await until(() => snapshot().text?.includes("KITTY_READY"), "Kitty mode applied");
  renderer.simulateKeyDown("c->c");
  command("commit", "c");
  renderer.simulateKeyDown("c->c", true);
  command("commit", "c");
  renderer.simulateKeyUp("c");
  await until(() => bytes().includes("\x1b[99;1:3u"), "Kitty release survives normalized input");
  assert.ok(bytes().includes("\x1b[99;1:2u"), "Kitty repeat survives normalized input");
  const beforePaste = bytes();
  command("paste", "P");
  await until(() => bytes() === beforePaste + "P", "native acknowledged paste");
  await until(
    () => events.some((e) => e.kind === "InputDelivery" && e.outcome === "Delivered"),
    "sole owner delivery receipt",
  );
  command("paste", "unsafe\n");
  await until(
    () => events.some((e) => e.kind === "InputDelivery" && e.outcome === "Refused"),
    "paste safety refusal",
  );
  const beforeStale = bytes();
  renderer.simulateKeyDown("cmd-s");
  renderer.simulateKeyUp("cmd-s");
  command("mark", "stale");
  const oldStream = viewState(view).stream;
  host.inputFixtureResync(client.handle);
  await until(
    () => viewState(view) && viewState(view).stream !== oldStream,
    "new replica identity",
  );
  command("commit", "late-old-ime");
  assert.match(snapshot().error, /StalePresentation|NotReady/);
  host.inputFixtureDestroyView(client.handle, view);
  command("commit", "must-not-send");
  assert.match(snapshot().error, /Engine|StalePresentation/);
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(bytes(), beforeStale, "destroyed view and app shortcut cannot send");
  closed = true;
  client.close();
  command("commit", "closed");
  assert.match(snapshot().error, /StaleHandle/);
  batch([["destroyElement", 2]]);
  console.log(
    JSON.stringify({
      result: "pass",
      evidence:
        "GPU key/mouse dispatch, IME callbacks and UTF16 graphemes, PTY exact bytes, Kitty repeat/release, application mouse versus Shift view-local selection/scroll, acknowledged paste/refusal, inactive/resync/destroyed/closed fences",
      bytes: bytes(),
      events: events.filter((e) => e.kind === "InputDelivery"),
    }),
  );
}
main()
  .catch((error) => {
    console.error(error);
    process.exitCode = 1;
  })
  .finally(() => {
    if (!closed) {
      closed = true;
      client.close();
    }
    fs.rmSync(temp, { recursive: true, force: true });
  });
