import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';
import { navigationRequest, navigationPage, navigationIntent, snapshot } from '../protocol.ts';

const bytes = text => new TextEncoder().encode(text);
const text = value => new TextDecoder().decode(value);
const revision = { hi: 0, lo: 7 };
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
function page(query = '', offset = 0, indices = [0, 1, 2, 3], total = 10, rev = revision) {
  const head = navigationRequest(rev, offset, bytes(query));
  const rows = indices.map(index => {
    const label = bytes(`Window 2 · Phux · terminal ${index}`);
    return [index % 256, Math.floor(index / 256), label.length, ...label];
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

test('new terminal and reconnect use the native adopted window', () => {
  for (const [kind, tag] of [['new_terminal', 2], ['reconnect', 12]]) {
    const [, cmd] = step(initialModel()[0], { kind });
    assert.equal(cmd.op, 'host_bytes');
    assert.equal(cmd.payload[1], tag);
    assert.equal(cmd.payload[11], 255);
  }
});

test('whole catalog is paged and selection carries unfiltered identity under its revision', () => {
  let model = open();
  [model] = step(model, { kind: 'navigation_loaded', body: page('', 0, [257, 400, 600, 700]) });
  assert.equal(model.paletteNext, true);
  assert.equal(model.palettePrevious, false);
  const [closed, cmd] = step(model, { kind: 'palette_pick', index: 400 });
  assert.equal(closed.paletteOpen, false);
  assert.deepEqual(cmd.payload, navigationIntent(revision, 400));
  const [loading, request] = step(model, { kind: 'palette_next' });
  assert.equal(loading.paletteRows.length, 0);
  assert.equal(loading.paletteOffset, 4);
  assert.equal(request.op, 'request');
  [model] = step(loading, { kind: 'navigation_loaded', body: page('', 4, [800, 900, 1000, 1100]) });
  assert.equal(model.palettePrevious, true);
  assert.equal(model.paletteNext, true);
  [model] = step(model, { kind: 'palette_next' });
  [model] = step(model, { kind: 'navigation_loaded', body: page('', 8, [2000, 3000]) });
  assert.equal(model.paletteNext, false);
  assert.deepEqual(step(model, { kind: 'palette_pick', index: 3000 })[1].payload, navigationIntent(revision, 3000));
});

test('stale revision, query, and page replies cannot replace current rows', () => {
  let model = { ...open(), paletteQuery: bytes('wanted'), paletteOffset: 4 };
  for (const body of [page('wanted', 4, [9, 10, 11, 12], 10, { hi: 0, lo: 6 }), page('other', 4), page('wanted', 0)]) {
    assert.equal(step(model, { kind: 'navigation_loaded', body })[0], model);
  }
  [model] = step(model, { kind: 'navigation_loaded', body: page('wanted', 4, [300], 5) });
  assert.equal(model.paletteRows[0].index, 300);
  assert.equal(step(model, { kind: 'palette_pick', index: 9 })[1], null);
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

test('catalog invalidation immediately withdraws selectable rows', () => {
  let model = open();
  [model] = step(model, { kind: 'navigation_loaded', body: page() });
  const event = new Uint8Array(18); event[0] = 1; event[1] = 1; event[2] = 2; event[10] = 8;
  [model] = step(model, { kind: 'engine_event', key: 0, state: 'data', bytes: event, droppedPending: 0, droppedTotal: 0 });
  assert.equal(model.paletteRows.length, 0);
  const stale = step(model, { kind: 'navigation_loaded', body: page() })[0];
  assert.equal(stale.paletteRows.length, 0);
  assert.equal(step(model, { kind: 'palette_pick', index: 0 })[1], null);
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
