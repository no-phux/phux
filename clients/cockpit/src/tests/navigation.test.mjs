import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';
import { navigationRequest, navigationPage, snapshot } from '../protocol.ts';

const bytes = text => new TextEncoder().encode(text);
const text = value => new TextDecoder().decode(value);
const revision = { hi: 0, lo: 7 };
function target(index) {
  const out = new Uint8Array(42);
  out[0] = 2;
  new DataView(out.buffer).setBigUint64(34, BigInt(index), true);
  return out;
}
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
function committedCommand(cmd) {
  assert.equal(cmd.op, 'batch');
  if (cmd.cmds[1].name === 'cockpit.navigation') {
    assert.equal(cmd.cmds.length, 3);
    assert.deepEqual(cmd.cmds[2], { op: 'cancel', key: 'cockpit-window-command' });
  } else assert.equal(cmd.cmds.length, 2);
  assert.deepEqual(cmd.cmds[0], { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  return cmd.cmds[1];
}
function page(query = '', offset = 0, indices = [0, 1, 2, 3], total = 10, rev = revision, resourceOffset = 0) {
  const head = navigationRequest(rev, offset, bytes(query));
  const rows = indices.map(index => {
    const label = bytes(`Window 2 · Phux · terminal ${index}`);
    const identity = target(index + resourceOffset);
    return [index % 256, Math.floor(index / 256), label.length, identity.length, 0, ...identity, ...label];
  }).flat();
  return new Uint8Array([...head, total % 256, Math.floor(total / 256), indices.length, ...rows]);
}
function open() {
  return step({ ...initialModel()[0], engineRevision: revision, engineConnected: true }, { kind: 'palette_open' })[0];
}
function snapshotBytes(connection) {
  // Empty main, no themes/path/secondary; connection is independent of READY.
  const out = new Uint8Array(33);
  out[0] = 1; out[1] = 2; out[10] = 7; out[23] = connection;
  out[26] = 168; out[29] = 255;
  return out;
}

test('engine readiness never implies a connected Phux provider', () => {
  for (const [state, label] of [[0, 'Local terminals'], [1, 'Phux connecting...'], [2, 'Phux connected'], [3, 'Phux offline']]) {
    const [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body: snapshotBytes(state) });
    assert.equal(model.engineConnected, true);
    assert.equal(text(model.status), 'READY');
    assert.equal(text(model.connectionStatus), label);
    assert.equal(model.canReconnect, state === 3);
  }
});

test('a terminal whose wheel another client holds says so in its window status', () => {
  const body = new Uint8Array([...snapshotBytes(2), 8, 0, 0, 0, 0]);
  const [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body });
  assert.equal(text(model.connectionStatus), 'Phux connected / Another client is driving');
  assert.equal(text(model.window1Status), 'Phux connected');
});

test('per-window terminal status survives connected and offline snapshot projection', () => {
  const body = new Uint8Array([...snapshotBytes(2), 6, 2, 3, 7, 0]);
  const [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body });
  assert.equal(text(model.connectionStatus), 'Phux connected / Loading earlier history');
  assert.equal(text(model.window1Status), 'Phux connected / Recovering terminal: waiting for snapshot');
  assert.equal(text(model.window2Status), 'Phux connected / Terminal frozen: waiting for recovery');
  assert.equal(text(model.window3Status), 'Phux connected / Earlier history available');
  assert.equal(text(model.window4Status), 'Phux connected');
  body[23] = 3;
  assert.equal(text(step(initialModel()[0], { kind: 'snapshot_loaded', body })[0].window1Status),
    'Phux offline / Recovering terminal: waiting for snapshot');
  assert.equal(snapshot(body.subarray(0, body.length - 1)), null);
  body[body.length - 1] = 9;
  assert.equal(snapshot(body), null);
});

test('new terminal and reconnect use the native adopted window', () => {
  const [, create] = step(initialModel()[0], { kind: 'new_terminal' });
  assert.equal(create.op, 'request');
  assert.equal(create.name, 'cockpit.tab-command');
  assert.equal(create.payload[1], 3);
  assert.equal(create.payload[11], 2);
  assert.equal(create.payload[21], 255);
  const [, reconnect] = step(initialModel()[0], { kind: 'reconnect' });
  assert.equal(reconnect.op, 'host_bytes');
  assert.equal(reconnect.payload[1], 12);
  assert.equal(reconnect.payload[11], 255);
});

test('an opened navigator lands on the engine current row', () => {
  const model = open();
  const head = navigationRequest(revision, 0, bytes(''));
  const indices = [10, 20, 30];
  const records = indices.flatMap(index => {
    const label = bytes(`terminal ${index}`);
    const identity = target(index);
    return [index % 256, Math.floor(index / 256), label.length, identity.length, 0, ...identity, ...label];
  });
  const flags = [1, 1, 3];
  const metadata = indices.flatMap((index, position) => [0, flags[position], 0]);
  const body = new Uint8Array([...head, 3, 0, 3, ...records, 0x4e, ...metadata]);
  const [loaded] = step(model, { kind: 'navigation_loaded', body });
  assert.equal(loaded.paletteCursor, 2);
  assert.equal(loaded.paletteRows[2].current, true);
  assert.equal(loaded.paletteRows[2].highlighted, true);
  assert.equal(loaded.paletteRows[0].highlighted, false);
  assert.deepEqual(loaded.paletteSelection, loaded.paletteRows[2].target);
});

test('transport pages accumulate into a scrollable catalog and preserve captured targets', () => {
  let model = open();
  let request;
  [model, request] = step(model, { kind: 'navigation_loaded', body: page('', 0, [257, 400, 600, 700]) });
  assert.equal(model.paletteLoading, true);
  assert.equal(model.palettePrevious, false);
  const [closed, cmd] = step(model, { kind: 'palette_pick', target: model.paletteRows[1].target });
  assert.equal(closed.paletteOpen, false);
  assert.equal(committedCommand(cmd).name, 'cockpit.tab-command');
  assert.equal(committedCommand(cmd).payload[1], 2);
  assert.deepEqual(committedCommand(cmd).payload.subarray(10), target(400));
  assert.equal(model.paletteRows.length, 4);
  assert.equal(model.paletteOffset, 4);
  assert.equal(request.op, 'request');
  assert.deepEqual(request.payload, navigationRequest(revision, 4, bytes('')));
  [model] = step(model, { kind: 'navigation_loaded', body: page('', 4, [800, 900, 1000, 1100]) });
  assert.equal(model.paletteRows.length, 8);
  assert.equal(model.paletteOffset, 8);
  [model] = step(model, { kind: 'navigation_loaded', body: page('', 8, [2000, 3000]) });
  assert.equal(model.paletteNext, false);
  assert.equal(model.paletteRows.length, 10);
  assert.deepEqual(committedCommand(step(model, { kind: 'palette_pick', target: model.paletteRows[9].target })[1]).payload.subarray(10), target(3000));
});

test('boot and every modality transition deliver committed context before effects', () => {
  const [initial, boot] = initialModel();
  const marker = { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() };
  assert.deepEqual(boot.cmds[0], marker);
  let palette = open();
  [palette] = step(palette, { kind: 'navigation_loaded', body: page() });
  const settings = step(initial, { kind: 'settings_open' })[0];
  const transitions = [
    [initial, { kind: 'palette_open' }, true, false],
    [palette, { kind: 'settings_open' }, false, true],
    [palette, { kind: 'palette_close' }, false, false],
    [palette, { kind: 'palette_submit' }, false, false],
    [palette, { kind: 'palette_pick', target: palette.paletteRows[0].target }, false, false],
  ];
  for (const [before, msg, paletteOpen, settingsOpen] of transitions) {
    const [after, command] = step(before, msg);
    assert.equal(after.paletteOpen, paletteOpen, msg.kind);
    assert.equal(after.settingsOpen, settingsOpen, msg.kind);
    assert.deepEqual(command.op === 'batch' ? command.cmds[0] : command, marker, msg.kind);
  }
  assert.equal(step(settings, { kind: 'settings_open' })[1], null);
  // Settings completion is acknowledged by its native transaction before the
  // overlay releases focus; appearance.test.mjs covers those transitions.
  assert.equal(step(palette, { kind: 'palette_pick', target: new Uint8Array() })[1], null);
});

test('stale revision, query, and page replies cannot replace current rows', () => {
  let model = { ...open(), paletteQuery: bytes('wanted'), paletteOffset: 4 };
  for (const body of [page('wanted', 4, [9, 10, 11, 12], 10, { hi: 0, lo: 6 }), page('other', 4), page('wanted', 0)]) {
    assert.equal(step(model, { kind: 'navigation_loaded', body })[0], model);
  }
  [model] = step(model, { kind: 'navigation_loaded', body: page('wanted', 4, [300], 5) });
  assert.equal(model.paletteRows[0].index, 300);
  assert.equal(step(model, { kind: 'palette_pick', target: new Uint8Array(299) })[1], null);
});

test('arrow navigation moves through accumulated pages without replacing preceding rows', () => {
  let model = open();
  [model] = step(model, { kind: 'navigation_loaded', body: page() });
  assert.equal(model.paletteOffset, 4);
  [model] = step(model, { kind: 'navigation_loaded', body: page('', 4, [4, 5, 6, 7], 8) });
  for (let index = 0; index < 4; index++) [model] = step(model, { kind: 'palette_move', delta: 1 });
  assert.equal(model.paletteRows[4].highlighted, true);
  [model] = step(model, { kind: 'palette_move', delta: -1 });
  assert.equal(model.paletteCursor, 3);
  assert.equal(model.paletteRows[3].highlighted, true);
  assert.equal(model.paletteRows[0].highlighted, false);
});

test('catalog invalidation fences page reads but cannot retarget a held painted action', () => {
  let model = open();
  [model] = step(model, { kind: 'navigation_loaded', body: page() });
  const held = model.paletteRows[1].target;
  const event = new Uint8Array(18); event[0] = 1; event[1] = 1; event[2] = 2; event[10] = 8;
  [model] = step(model, { kind: 'engine_event', key: 0, state: 'data', bytes: event, droppedPending: 0, droppedTotal: 0 });
  assert.equal(model.paletteRows.length, 4);
  const stale = step(model, { kind: 'navigation_loaded', body: page() })[0];
  assert.equal(stale.paletteRows.length, 4);
  assert.equal(step(model, { kind: 'palette_submit' })[1], null);
  const [, command] = step(model, { kind: 'palette_pick', target: held });
  assert.deepEqual(committedCommand(command).payload.subarray(10), target(1));
});

test('held catalog target survives filtering and replacement rows and owns its bytes', () => {
  const body = page();
  let [model] = step(open(), { kind: 'navigation_loaded', body });
  const held = model.paletteRows[2].target;
  body.fill(255);
  [model] = step(model, { kind: 'palette_edit', edit: { kind: 'insert_text', text: bytes('other') } });
  model = { ...model, paletteQuery: bytes('other'), paletteOffset: 0, paletteLoading: true };
  [model] = step(model, { kind: 'navigation_loaded', body: page('other', 0, [900], 1) });
  const [, command] = step(model, { kind: 'palette_pick', target: held });
  assert.deepEqual(committedCommand(command).payload.subarray(10), target(2));
  const [, keyboard] = step(model, { kind: 'palette_submit' });
  assert.deepEqual(committedCommand(keyboard).payload.subarray(10), target(900));
});

test('an unavailable engine withdraws claims about Phux connectivity', () => {
  let [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  [model] = step(model, { kind: 'snapshot_failed', error: bytes('unavailable') });
  assert.equal(model.engineConnected, false);
  assert.equal(text(model.connectionStatus), 'Connection status unavailable');
  assert.equal(model.canReconnect, false);
});

test('bounded navigation packets reject truncation and overflow', () => {
  const valid = page();
  assert.equal(navigationPage(valid).rows.length, 4);
  for (let length = 0; length < valid.length; length++) assert.equal(navigationPage(valid.subarray(0, length)), null);
  assert.equal(navigationPage(new Uint8Array(4097)), null);
  assert.equal(snapshot(snapshotBytes(3)).connection, 3);
});

// One tab record: id 1, no attention, the given title, no cwd. The main
// section starts at 28 and the trailer that follows it is themes/settings.
function tabbedSnapshotBytes(title = 'Terminal 1', extension = []) {
  const label = bytes(title);
  const head = new Uint8Array(28);
  head[0] = 1; head[1] = 2; head[10] = 7;
  head[20] = 1; head[21] = 0; head[23] = 2; head[24] = 0; head[25] = 1; head[26] = 168;
  const tab = [1, 0, 0, 0, 0, label.length, 0, ...label];
  // No themes, active theme 255, no config flags, no path, no secondary
  // windows, then the five per-window terminal states.
  const trailer = [0, 255, 0, 0, 0, 0, 0, 0, 0, 0];
  return new Uint8Array([...head, ...tab, ...trailer, ...extension]);
}

test('published snapshots show confirmed tabs without another snapshot host request', () => {
  const body = tabbedSnapshotBytes('Confirmed pane');
  body[2] = 1;
  const event = { kind: 'engine_event', key: 0, state: 'data', bytes: body, droppedPending: 0, droppedTotal: 0 };
  // Complete boot's result read so this publication starts a fresh one.
  let [model] = step(initialModel()[0], { kind: 'command_result_loaded', body: new Uint8Array([1, 0]) });
  let command;
  [model, command] = step(model, event);
  assert.equal(text(model.tabs[0].title), 'Confirmed pane');
  assert.equal(model.engineConnected, true);
  const requests = command.op === 'batch' ? command.cmds : [command];
  assert.equal(requests.some(item => item.name === 'cockpit.snapshot'), false);
  assert.equal(requests.some(item => item.name === 'cockpit.command-results'), true);

  // A queued older host completion or channel publication cannot restore
  // an obsolete topology after the direct publication.
  const stale = tabbedSnapshotBytes('Obsolete pane');
  [model, command] = step(model, { ...event, bytes: stale });
  assert.equal(text(model.tabs[0].title), 'Confirmed pane');
  assert.equal(command, null);
  [model, command] = step(model, { kind: 'snapshot_loaded', body: stale });
  assert.equal(text(model.tabs[0].title), 'Confirmed pane');
  assert.equal(command, null);
});

test('a pushed snapshot refreshes open navigation and preserves an in-flight result read', () => {
  let [model] = step(open(), { kind: 'navigation_loaded', body: page() });
  const body = snapshotBytes(2);
  body[2] = 1; body[10] = 8;
  const event = { kind: 'engine_event', key: 0, state: 'data', bytes: body, droppedPending: 0, droppedTotal: 0 };
  let command;
  [model, command] = step(model, event);
  const requests = command.op === 'batch' ? command.cmds : [command];
  const navigation = requests.find(item => item.name === 'cockpit.navigation');
  assert.deepEqual(navigation.payload, navigationRequest({ hi: 0, lo: 8 }, 0, bytes('')));
  assert.equal(model.paletteLoading, true);
  // A complete one-row page whose stable resource differs from the old row.
  [model] = step(model, { kind: 'navigation_loaded', body: page('', 0, [0], 1, { hi: 0, lo: 8 }, 4) });
  assert.deepEqual(model.paletteRows[0].target, target(4));

  body[2] = 2;
  [model, command] = step(model, event);
  const pending = command.op === 'batch' ? command.cmds : [command];
  assert.equal(pending.some(item => item.name === 'cockpit.command-results'), false);
  [model, command] = step(model, { kind: 'command_result_loaded', body: new Uint8Array([1, 0]) });
  assert.equal(command.name, 'cockpit.command-results');
  assert.equal(model.commandResults.loading, true);
});

// `[kind][u16 length][payload]`, the framing ts_snapshot.zig writes.
function extensionRecord(kind, payload) {
  return [kind, payload.length % 256, Math.floor(payload.length / 256), ...payload];
}

test('the rail is one row per tab and carries only the tab attention mark', () => {
  const quiet = tabbedSnapshotBytes('Terminal 1');
  let [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body: quiet });
  assert.deepEqual(model.railRows.map(row => text(row.label)), ['Terminal 1']);
  assert.deepEqual(model.railRows.map(row => text(row.mark)), ['']);
  assert.equal('agents' in model.visibleTabs[0], false);

  // A blocked agent reaches the chrome only as its tab's attention bit.
  const asking = tabbedSnapshotBytes('Terminal 1');
  asking[28 + 4] = 1;
  [model] = step(model, { kind: 'snapshot_loaded', body: asking });
  assert.equal(model.railRows.length, 1);
  assert.deepEqual(model.railRows.map(row => text(row.mark)), ['●']);
  assert.deepEqual(model.railRows.map(row => text(row.attentionLabel)), ['Needs attention: Terminal 1']);
});

test('an unknown extension kind is stepped over, and a malformed known one is refused', () => {
  // Kinds 1 and 6 (retired agent rows) are as unknown as 200 and 201.
  const unknown = tabbedSnapshotBytes('Terminal 1', [
    ...extensionRecord(200, [9, 9, 9]),
    ...extensionRecord(1, [1, 0, 0, 3, 0, 0]),
    ...extensionRecord(6, [0, 0, 0]),
    ...extensionRecord(201, []),
  ]);
  const decoded = snapshot(unknown);
  assert.notEqual(decoded, null);
  assert.equal(decoded.tabs.length, 1);

  // A tab-context record of the wrong length, a length that outruns the
  // snapshot, and a truncated header.
  assert.equal(snapshot(tabbedSnapshotBytes('Terminal 1', extensionRecord(2, [1, 2, 3]))), null);
  assert.equal(snapshot(tabbedSnapshotBytes('Terminal 1', [2, 40, 0, 0])), null);
  assert.equal(snapshot(tabbedSnapshotBytes('Terminal 1', [2, 0])), null);
});
