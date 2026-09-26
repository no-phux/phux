// Actual NAPI exports against one isolated PTY. No JS frame/cell transport.
const assert = require('node:assert/strict');
const { DesktopClient } = require(process.argv[2]);
const socketPath = process.argv[3];
const client = new DesktopClient();
const events = [];
let closed = false;

async function until(predicate, description) {
  const deadline = Date.now() + 20_000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, `timeout: ${description}; status=${client.status()}; error=${client.lastError()}`);
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

function selectRow(view, row) {
  const start = client.trackViewAnchor(view, { space: 1, column: 0, row });
  const end = client.trackViewAnchor(view, { space: 1, column: 79, row });
  client.setViewSelection(view, start, end, false);
  return { start, end, text: client.viewSelectionText(view) };
}

const key = { key: 58, action: 1, mods: 0, consumedMods: 0 };

function validateNumbers(view) {
  for (const bad of [NaN, Infinity, -1, 0.5, 2 ** 32]) {
    assert.throws(() => client.resizeView(view, bad, 24));
    assert.throws(() => client.resizeView(view, 80, bad));
  }
  assert.equal(client.resizeView(view, 0, 24), 'InvalidSize');
  assert.equal(client.resizeView(view, 80, 65536), 'InvalidSize');
  assert.throws(() => client.attachTerminalPreservingGeometry('invalid'), /InvalidResourceId/);
  for (const bad of [NaN, Infinity, -Infinity, 0.5, 2 ** 53]) {
    assert.throws(() => client.scrollView(view, bad), /InvalidInteger/);
  }
  for (const bad of [-1, 65536, 2 ** 32, NaN, 1.5]) {
    assert.throws(() => client.trackViewAnchor(view, { space: 1, column: bad, row: 0 }));
  }
  for (const bad of [-1, 1024, 65536, 2 ** 32, NaN, 1.5]) {
    assert.throws(() => client.keyEvent(view, { ...key, mods: bad }));
  }
  for (const bad of [-1, 2 ** 32, NaN, 1.5, 9999]) {
    assert.throws(() => client.keyEvent(view, { ...key, key: bad }));
  }
  assert.throws(() => client.keyEvent(view, { ...key, consumedMods: 1 }), /ConsumedModifiersNotHeld/);
  assert.throws(() => client.keyEvent(view, { ...key, unshiftedCodepoint: 0xd800 }), /InvalidCodepoint/);
  assert.throws(() => client.keyEvent(view, { ...key, text: '\n' }), /InvalidKeyText/);
  assert.throws(() => client.keyEvent(view, { ...key, composing: true }), /CompositionRequiresNativeImeBridge/);
  assert.throws(() => client.commitText(view, '\n'), /InvalidKeyText/);
  assert.throws(() => client.commitText(view, 'x'.repeat(4097)), /TextLimitExceeded/);
  assert.throws(() => client.mouseEvent(view, { action: 0, button: 1, mods: 0, x: Infinity, y: 0 }), /InvalidCoordinate/);
  assert.throws(() => client.searchView(view, 'x'.repeat(4097), true), /TextLimitExceeded/);
  for (const bad of ['0', '01', '+1', '1e2', '18446744073709551616']) {
    assert.throws(() => client.viewInfo(bad), /InvalidHandle/);
  }
}

async function delivery(id, outcome) {
  assert.equal(typeof id, 'string');
  await until(() => events.some(event => event.deliveryId === id), 'input delivery');
  assert.equal(events.find(event => event.deliveryId === id).outcome, outcome);
}

function dimensions(owner, view) {
  const { cols, rows } = owner.viewInfo(view);
  return [cols, rows];
}

async function geometrySmoke(view, pane) {
  const request = client.spawnTerminal(pane.sessionId);
  await until(() => events.some(e => e.kind === 'SpawnAnswered' && e.requestId === request), 'second PTY spawn');
  const otherTerminal = events.find(e => e.kind === 'SpawnAnswered' && e.requestId === request).terminalId;
  assert.ok(otherTerminal);
  assert.notEqual(otherTerminal, pane.terminalId);
  await until(() => client.inputReadiness(otherTerminal).ready, 'second PTY ready');
  const otherView = client.createView(otherTerminal);
  const otherSize = dimensions(client, otherView);
  assert.equal(client.resizeView(view, 100, 30), 'Queued');
  await until(() => dimensions(client, view).join() === '100,30', 'authoritative targeted resize');
  assert.deepEqual(dimensions(client, otherView), otherSize, 'other terminal never resizes');
  const sibling = client.createView(pane.terminalId);
  assert.deepEqual(dimensions(client, sibling), [100, 30], 'duplicate view uses canonical geometry');

  // A separate small-viewport client subscribes without a session ATTACH or
  // global viewport resize. Its duplicate must preserve the existing PTY size.
  const duplicate = new DesktopClient();
  let duplicateClosed = false;
  try {
    duplicate.connect({ socketPath, cols: 5, rows: 2 }, () => {
      if (!duplicateClosed) duplicate.takeEvents();
    });
    await until(() => duplicate.status() === 'Attached', 'geometry-neutral client negotiation');
    assert.notEqual(duplicate.attachTerminalPreservingGeometry(pane.terminalId), 0);
    await until(() => duplicate.inputReadiness(pane.terminalId).ready, 'preserving resource subscription');
    const duplicateView = duplicate.createView(pane.terminalId);
    assert.deepEqual(dimensions(duplicate, duplicateView), [100, 30]);
    assert.deepEqual(dimensions(client, view), [100, 30]);
    assert.deepEqual(dimensions(client, otherView), otherSize);
    assert.equal(duplicate.attachTerminalPreservingGeometry(pane.terminalId), 0, 'already admitted');
    duplicate.destroyView(duplicateView);
  } finally {
    duplicateClosed = true;
    duplicate.close();
  }
  const observer = new DesktopClient();
  let observerClosed = false;
  const observerEvents = [];
  try {
    observer.connect({ socketPath, cols: 5, rows: 2, observer: true }, () => {
      if (!observerClosed) observerEvents.push(...observer.takeEvents());
    });
    await until(() => observer.status() === 'Attached', 'observer negotiation');
    const request = observer.attachTerminalPreservingGeometry(pane.terminalId);
    assert.notEqual(request, 0);
    await until(() => observerEvents.some(e => e.kind === 'AttachAnswered' && e.requestId === request), 'observer subscription');
    const observed = observer.createView(pane.terminalId);
    assert.deepEqual(dimensions(observer, observed), [100, 30]);
    assert.equal(observer.resizeView(observed, 42, 12), 'Observer');
    assert.equal(observer.commitText(observed, 'must not type'), false);
    assert.deepEqual(dimensions(client, view), [100, 30]);
    assert.deepEqual(dimensions(client, otherView), otherSize);
    observer.destroyView(observed);
  } finally {
    observerClosed = true;
    observerEvents.push(...observer.close());
  }
  client.destroyView(sibling);
  client.destroyView(otherView);
}

async function main() {
  client.connect({ socketPath, cols: 80, rows: 24, sessionName: 'napi-view-smoke' }, () => {
    if (!closed) events.push(...client.takeEvents());
  });
  await until(() => client.topology()?.sessions.some(s => s.name === 'napi-view-smoke'), 'topology');
  await until(() => client.status() === 'Attached', 'session attach');
  const pane = client.topology().panes.find(p => p.sessionName === 'napi-view-smoke');
  assert.ok(pane);
  await until(() => client.inputReadiness(pane.terminalId).ready, 'input readiness');
  const first = client.createView(pane.terminalId);
  const second = client.createView(pane.terminalId);
  assert.match(first, /^[1-9][0-9]*$/);
  assert.notEqual(first, second);
  assert.equal(client.viewInfo(first).terminalId, pane.terminalId);
  assert.equal(client.viewInfo(second).terminalId, pane.terminalId);
  validateNumbers(first);
  assert.equal(client.focusView(first, true), true);
  assert.equal(client.mouseEvent(first, { action: 2, button: 0, mods: 0, x: 1.5, y: 2.5 }), true);

  // The command is typed through normalized key events into the same real PTY.
  assert.equal(client.commitText(first, "printf '\\033[2J\\033[H'; i=0; while [ $i -lt 120 ]; do printf 'VIEW-LINE-%03d\\n' $i; i=$((i+1)); done"), true);
  assert.equal(client.keyEvent(first, key), true);
  await until(() => BigInt(client.viewInfo(second).scrollTotal) >= 120n, 'shared PTY output');
  // Completion observed in the actual view engine, not a synthetic frame.
  await until(() => client.searchView(second, 'VIEW-LINE-119', true).length === 1, 'last output row');

  client.scrollView(first, -1000);
  const before = client.viewInfo(second);
  assert.equal(client.viewInfo(first).atTail, false);
  assert.equal(before.atTail, true);
  assert.notEqual(client.viewInfo(first).scrollOffset, before.scrollOffset);
  assert.equal(before.cols, 80);
  assert.equal(before.rows, 24);
  assert.equal(typeof before.generation, 'string');
  assert.equal(typeof before.streamId, 'string');
  assert.equal(typeof before.bootstrapId, 'string');
  assert.equal('cells' in before, false);
  assert.equal(typeof client.acquireView, 'undefined');
  assert.equal(typeof client.acknowledgeProjection, 'undefined');
  assert.equal(typeof client.acknowledgeProjectionIf, 'undefined');

  const old = selectRow(first, 2);
  const live = selectRow(second, 2);
  assert.match(old.text, /VIEW-LINE-/);
  assert.match(live.text, /VIEW-LINE-/);
  assert.notEqual(old.text, live.text);
  assert.equal(client.viewSelectionText(first), old.text);
  assert.throws(() => client.setViewSelection(second, old.start, old.end, false));
  assert.throws(() => client.pinViewportView(second, old.start));
  assert.throws(() => client.releaseViewAnchor(second, old.start));
  assert.equal(client.viewSelectionText(first), old.text);

  const hits = client.searchView(first, 'VIEW-LINE-010', true);
  assert.equal(hits.length, 1);
  client.setViewSelection(first, hits[0].start, hits[0].end, false);
  assert.equal(client.viewSelectionText(first), 'VIEW-LINE-010');
  assert.equal(client.viewSelectionText(second), live.text);
  const unused = client.searchView(first, 'VIEW-LINE-011', true)[0];
  client.searchView(first, 'VIEW-LINE-012', true);
  assert.throws(() => client.setViewSelection(first, unused.start, unused.end, false), /anchor/);
  assert.equal(client.viewSelectionText(first), 'VIEW-LINE-010', 'active search selection survives replacement');
  client.clearViewSelection(first);
  client.releaseViewAnchor(first, hits[0].start);
  client.releaseViewAnchor(first, hits[0].end);
  client.followLiveView(first);
  assert.equal(client.viewInfo(first).atTail, true);

  const gesture = { phase: 0, clicks: 1, column: 0, row: 2, rectangle: false, x: 0, y: 32, columns: 80, cellWidth: 8, screenHeight: 384, paddingLeft: 0 };
  for (const field of ['phase', 'clicks', 'column', 'row', 'columns', 'cellWidth', 'screenHeight', 'paddingLeft']) {
    for (const bad of [NaN, Infinity, -1, 0.5, 2 ** 32]) {
      assert.throws(() => client.viewSelectionGesture(first, { ...gesture, [field]: bad }));
    }
  }
  assert.throws(() => client.viewSelectionGesture(first, { ...gesture, x: NaN }), /InvalidCoordinate/);
  assert.throws(() => client.viewSelectionGesture(first, { ...gesture, y: Infinity }), /InvalidCoordinate/);
  const press = client.viewSelectionGesture(first, gesture);
  assert.match(press.handle, /^[1-9][0-9]*$/);
  assert.throws(() => client.viewSelectionGesture(second, { ...gesture, phase: 1, handle: press.handle }));
  client.viewSelectionGesture(first, { ...gesture, phase: 1, handle: press.handle, column: 5, x: 40 });
  client.viewSelectionGesture(first, { ...gesture, phase: 2, handle: press.handle, column: 5, x: 40 });

  await delivery(client.pasteView(second, '# same PTY paste'), 'Delivered');
  await delivery(client.pasteView(second, 'untrusted newline\n'), 'Refused');
  client.destroyView(first);
  assert.throws(() => client.viewInfo(first), /StaleView/);
  assert.throws(() => client.scrollView(first, -1), /stale/);
  assert.throws(() => client.destroyView(first), /stale/);
  assert.throws(() => client.commitText(first, 'discard'), /[Ss]tale/);
  assert.equal(client.inputReadiness(pane.terminalId).ready, true);
  assert.equal(client.viewInfo(second).terminalId, pane.terminalId);
  assert.equal(client.viewSelectionText(second), live.text);
  await delivery(client.pasteView(second, ' sibling survives'), 'Delivered');
  const replacement = client.createView(pane.terminalId);
  assert.notEqual(replacement, first);
  assert.notEqual(replacement, second);
  client.destroyView(replacement);
  assert.equal(events.some(event => event.kind === 'SpawnAnswered'), false, 'views never create a PTY');
  await geometrySmoke(second, pane);
  client.destroyView(second);
  closed = true;
  client.close();
  assert.throws(() => client.viewInfo(second), /StaleHandle/);
  console.log('NAPI view smoke passed: shared PTY views, independent scroll/selection/search/gestures, input, bounded copy, stale handles, sibling lifetime, targeted two-PTY geometry, preserving subscription and observer refusal');
}

main().catch(error => {
  console.error(error);
  closed = true;
  try { client.close(); } catch {}
  process.exitCode = 1;
});
