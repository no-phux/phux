import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';
import { navigationRequest, navigationAgentsRequest, navigationIntent, navigationPage, snapshot } from '../protocol.ts';

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
  assert.equal(cmd.cmds.length, 2);
  assert.deepEqual(cmd.cmds[0], { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  return cmd.cmds[1];
}
function page(query = '', offset = 0, indices = [0, 1, 2, 3], total = 10, rev = revision) {
  const head = navigationRequest(rev, offset, bytes(query));
  const rows = indices.map(index => {
    const label = bytes(`Window 2 · Phux · terminal ${index}`);
    const identity = target(index);
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
  body[body.length - 1] = 8;
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

test('whole catalog is paged and selection echoes the captured opaque target', () => {
  let model = open();
  [model] = step(model, { kind: 'navigation_loaded', body: page('', 0, [257, 400, 600, 700]) });
  assert.equal(model.paletteNext, true);
  assert.equal(model.palettePrevious, false);
  const [closed, cmd] = step(model, { kind: 'palette_pick', target: model.paletteRows[1].target });
  assert.equal(closed.paletteOpen, false);
  assert.equal(committedCommand(cmd).name, 'cockpit.tab-command');
  assert.equal(committedCommand(cmd).payload[1], 2);
  assert.deepEqual(committedCommand(cmd).payload.subarray(10), target(400));
  const [loading, request] = step(model, { kind: 'palette_next' });
  assert.equal(loading.paletteRows.length, 0);
  assert.equal(loading.paletteOffset, 4);
  assert.equal(committedCommand(request).op, 'request');
  assert.deepEqual(committedCommand(request).payload, navigationRequest(revision, 4, bytes('')));
  [model] = step(loading, { kind: 'navigation_loaded', body: page('', 4, [800, 900, 1000, 1100]) });
  assert.equal(model.palettePrevious, true);
  assert.equal(model.paletteNext, true);
  [model] = step(model, { kind: 'palette_next' });
  [model] = step(model, { kind: 'navigation_loaded', body: page('', 8, [2000, 3000]) });
  assert.equal(model.paletteNext, false);
  assert.deepEqual(committedCommand(step(model, { kind: 'palette_pick', target: model.paletteRows[1].target })[1]).payload.subarray(10), target(3000));
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

test('arrow navigation crosses page boundaries in reading order', () => {
  let model = open();
  [model] = step(model, { kind: 'navigation_loaded', body: page() });
  for (let index = 0; index < 4; index++) [model] = step(model, { kind: 'palette_move', delta: 1 });
  assert.equal(model.paletteOffset, 4);
  [model] = step(model, { kind: 'navigation_loaded', body: page('', 4, [4, 5, 6, 7]) });
  [model] = step(model, { kind: 'palette_move', delta: -1 });
  assert.equal(model.paletteOffset, 0);
  [model] = step(model, { kind: 'navigation_loaded', body: page() });
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
  assert.equal(model.paletteRows.length, 0);
  const stale = step(model, { kind: 'navigation_loaded', body: page() })[0];
  assert.equal(stale.paletteRows.length, 0);
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

// `[kind][u16 length][payload]`, the framing ts_snapshot.zig writes.
function extensionRecord(kind, payload) {
  return [kind, payload.length % 256, Math.floor(payload.length / 256), ...payload];
}

// `[window][tab][state][flags][provider length][provider]`.
function agentRow(window, tab, state, attention, provider) {
  const slug = bytes(provider);
  return [window, tab, state, attention ? 1 : 0, slug.length, ...slug];
}

test('agent rows decode from the extension record and hang under their tab', () => {
  const payload = [2, ...agentRow(0, 0, 1, false, 'claude'), ...agentRow(0, 0, 2, true, 'codex')];
  const body = tabbedSnapshotBytes('Terminal 1', extensionRecord(1, payload));
  const decoded = snapshot(body);
  assert.equal(decoded.agents.length, 2);
  assert.deepEqual(decoded.agents[0], { window: 0, tab: 0, state: 1, attention: false, provider: decoded.agents[0].provider,
    resource: bytes(''), parent: bytes(''), parentIndex: 65535 });
  assert.equal(text(decoded.agents[0].provider), 'claude');

  const [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body });
  const rows = model.visibleTabs[0].agents;
  assert.equal(rows.length, 2);
  assert.deepEqual(rows.map(row => text(row.provider)), ['claude', 'codex']);
  assert.deepEqual(rows.map(row => text(row.state)), ['working', 'blocked']);
  assert.deepEqual(rows.map(row => row.attention), [false, true]);

  // The rail draws the tab, then its agents indented under it. The blocked
  // one carries the quiet marker; nothing else on the rail does.
  assert.deepEqual(model.railRows.map(row => row.agent), [false, true, true]);
  assert.deepEqual(model.railRows.map(row => text(row.label)), ['Terminal 1', 'claude', 'codex']);
  assert.deepEqual(model.railRows.map(row => text(row.state)), ['', 'working', 'blocked']);
  assert.deepEqual(model.railRows.map(row => text(row.mark)), ['', '', '\u25cf']);
  // Every agent row names the tab a press would select, and takes none itself.
  assert.deepEqual(model.railRows.map(row => row.index), [0, 0, 0]);
  assert.deepEqual(model.railRows.map(row => row.selected), [true, false, false]);
});

test('an agent row is gone from the view the moment the next snapshot omits it', () => {
  const withRow = tabbedSnapshotBytes('Terminal 1', extensionRecord(1, [1, ...agentRow(0, 0, 2, true, 'claude')]));
  let [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body: withRow });
  assert.equal(model.visibleTabs[0].agents.length, 1);
  assert.equal(model.railRows.length, 2);
  [model] = step(model, { kind: 'snapshot_loaded', body: tabbedSnapshotBytes() });
  assert.equal(model.visibleTabs[0].agents.length, 0);
  assert.deepEqual(model.railRows.map(row => row.agent), [false]);
  assert.equal(text(model.status), 'READY');
});

test('offline and delayed snapshots cannot leave an agent claiming current blocked attention', () => {
  const body = tabbedSnapshotBytes('Terminal 1', extensionRecord(1, [1, ...agentRow(0, 0, 2, true, 'claude')]));
  body[23] = 3;
  let [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body });
  assert.equal(model.visibleTabs[0].agents[0].attention, false);
  assert.match(text(model.visibleTabs[0].agents[0].state), /offline/);
  [model] = step(model, { kind: 'snapshot_failed', error: bytes('unavailable') });
  assert.equal(model.railRows.some(row => row.agent), false);
});

const u16 = value => [value % 256, Math.floor(value / 256)];
function boundAgent(resource, parent, parentIndex, window = 0, tab = 0) {
  const provider = bytes('claude');
  return [window, tab, ...u16(parentIndex), 2, 1, provider.length, ...u16(bytes(resource).length),
    ...u16(bytes(parent).length), ...provider, ...bytes(resource), ...bytes(parent)];
}
function inspectedAgent(offset = 0, total = 30, parentIndex = 300, rev = revision) {
  const head = navigationAgentsRequest(rev, offset);
  const label = bytes('claude · blocked');
  const fields = [`phux:0:${9000 + offset}@`, 'phux:0:42@', `producer-session-${offset}`, 'Catalog: working; records: blocked'];
  return new Uint8Array([...head, ...u16(total), 1, ...u16(parentIndex), label.length, ...label,
    ...fields.flatMap(value => [...u16(bytes(value).length), ...bytes(value)])]);
}

test('identity rows distinguish split parents and jump using the exact fenced parent target', () => {
  const extension = extensionRecord(5, [...u16(30), 2,
    ...boundAgent('phux:0:9001@', 'phux:0:42@', 300),
    ...boundAgent('phux:0:9002@', 'phux:0:43@', 301)]);
  const body = tabbedSnapshotBytes('Split', extension);
  const decoded = snapshot(body);
  assert.equal(decoded.agentTotal, 30);
  assert.deepEqual(decoded.agents.map(row => text(row.parent)), ['phux:0:42@', 'phux:0:43@']);
  let [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body });
  assert.equal(text(model.agentCountLabel), 'Agents 30');
  assert.match(text(model.railRows[2].label), /phux:0:9002@ under phux:0:43@/);
  assert.deepEqual(step(model, { kind: 'agent_parent', index: 301 })[1].payload, navigationIntent(revision, 301));
  const event = new Uint8Array(18); event[0] = 1; event[1] = 1; event[10] = 8;
  [model] = step(model, { kind: 'engine_event', key: 0, state: 'data', bytes: event, droppedPending: 0, droppedTotal: 0 });
  assert.equal(model.railRows.some(row => row.agent), false);
  assert.equal(step(model, { kind: 'agent_parent', index: 301 })[1], null);
  assert.equal(snapshot(tabbedSnapshotBytes('Split', extensionRecord(5, [...u16(30), 0]))).agentTotal, 30);
  for (let end = body.length - extension.length + 1; end < body.length; end++) {
    assert.equal(snapshot(body.subarray(0, end)), null);
  }
});

test('complete agent inspector pages beyond snapshot cap with real identity and evidence in every window', () => {
  for (let window = 0; window < 5; window++) {
    let model = { ...initialModel()[0], engineRevision: revision, engineConnected: true, activeWindow: window };
    let cmd;
    [model, cmd] = step(model, { kind: 'agents_open' });
    assert.equal(model[window === 0 ? 'mainAgentsOpen' : `window${window}AgentsOpen`], true);
    assert.equal(model.mainPaletteOpen, false);
    assert.deepEqual(committedCommand(cmd).payload, navigationAgentsRequest(revision, 0));
    for (let offset = 0; offset < 30; offset++) {
      [model] = step(model, { kind: 'navigation_loaded', body: inspectedAgent(offset) });
      assert.equal(text(model.paletteRows[0].resource), `phux:0:${9000 + offset}@`);
      assert.equal(text(model.paletteRows[0].parent), 'phux:0:42@');
      assert.equal(text(model.paletteRows[0].nativeId), `producer-session-${offset}`);
      assert.equal(text(model.paletteRows[0].evidence), 'Catalog: working; records: blocked');
      assert.match(text(model.paletteNotice), new RegExp(`Agent ${offset + 1} of 30`));
      if (offset < 29) [model] = step(model, { kind: 'palette_move', delta: 1 });
    }
    assert.equal(model.paletteNext, false);
    assert.deepEqual(committedCommand(step(model, { kind: 'palette_submit' })[1]).payload, navigationIntent(revision, 300));
  }
});

test('agent inspector rejects delayed pages and cannot jump an absent parent', () => {
  let model = step({ ...initialModel()[0], engineRevision: revision, engineConnected: true }, { kind: 'agents_open' })[0];
  assert.equal(step(model, { kind: 'navigation_loaded', body: page() })[0], model);
  assert.equal(step(model, { kind: 'navigation_loaded', body: inspectedAgent(0, 30, 300, { hi: 0, lo: 6 }) })[0], model);
  [model] = step(model, { kind: 'navigation_loaded', body: inspectedAgent(0, 30, 65535) });
  assert.equal(step(model, { kind: 'palette_submit' })[1], null);
  const body = inspectedAgent();
  for (let end = 0; end < body.length; end++) assert.equal(navigationPage(body.subarray(0, end)), null);
});

test('automatic agent refresh never retargets inspection after catalog replacement', () => {
  let model = step({ ...initialModel()[0], engineRevision: revision, engineConnected: true }, { kind: 'agents_open' })[0];
  [model] = step(model, { kind: 'navigation_loaded', body: inspectedAgent() });
  assert.equal(text(model.inspectedResource), 'phux:0:9000@');
  const replacement = new Uint8Array(inspectedAgent());
  const at = Buffer.from(replacement).indexOf(Buffer.from('phux:0:9000@'));
  assert.ok(at > 0);
  replacement[at + 10] = '1'.charCodeAt(0);
  [model] = step(model, { kind: 'navigation_loaded', body: replacement });
  assert.equal(model.paletteRows.length, 0);
  assert.match(text(model.paletteNotice), /changed or closed/);
  assert.equal(step(model, { kind: 'palette_submit' })[1], null);
  [model] = step(model, { kind: 'palette_retry' });
  [model] = step(model, { kind: 'navigation_loaded', body: replacement });
  assert.equal(model.paletteRows.length, 1);
  assert.equal(text(model.inspectedResource), 'phux:0:9001@');
});

test('an unknown extension kind is stepped over, and a malformed known one is refused', () => {
  const unknown = tabbedSnapshotBytes('Terminal 1', [
    ...extensionRecord(200, [9, 9, 9]),
    ...extensionRecord(1, [1, ...agentRow(0, 0, 3, false, 'claude')]),
    ...extensionRecord(201, []),
  ]);
  const decoded = snapshot(unknown);
  assert.equal(decoded.agents.length, 1);
  assert.equal(decoded.agents[0].state, 3);

  // A row count that outruns its own record, a length that outruns the
  // snapshot, a state outside the closed vocabulary, and a truncated header.
  assert.equal(snapshot(tabbedSnapshotBytes('Terminal 1', extensionRecord(1, [2, ...agentRow(0, 0, 1, false, 'claude')]))), null);
  assert.equal(snapshot(tabbedSnapshotBytes('Terminal 1', [1, 40, 0, 0])), null);
  assert.equal(snapshot(tabbedSnapshotBytes('Terminal 1', extensionRecord(1, [1, ...agentRow(0, 0, 5, false, 'claude')]))), null);
  assert.equal(snapshot(tabbedSnapshotBytes('Terminal 1', [1, 0])), null);
});

test('a snapshot with no agent rows is byte-identical to one from before the kind existed', () => {
  const body = tabbedSnapshotBytes();
  const decoded = snapshot(body);
  assert.equal(decoded.agents.length, 0);
  assert.equal(decoded.tabs.length, 1);
});
