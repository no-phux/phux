import { test } from 'node:test';
import assert from 'node:assert/strict';
import { navigationRequest, navigationScopedRequest, navigationPage, navigationIntent, navigationHostFilter, snapshot } from '../protocol.ts';
import { initialModel, update } from '../core.ts';

const bytes = text => new TextEncoder().encode(text);
const text = value => new TextDecoder().decode(value);
const revision = { hi: 0x12345678, lo: 0xfedcba98 };
const empty = new Uint8Array();
const detail = 'Window 2 · /work';
// Opaque to TS: a catalog target begins with tag 2 and carries full identity.
function catalogTarget(identity, host = empty) {
  const out = new Uint8Array(43 + host.length);
  out[0] = 2;
  out[1] = 1;
  new DataView(out.buffer).setUint32(38, identity, true);
  out[42] = host.length;
  out.set(host, 43);
  return out;
}
const hostToken = host => new Uint8Array([3, host.length, ...host]);
const rowTarget = row => (row.kind === 3 ? hostToken(row.host) : catalogTarget(row.identity, row.host));
function page(scope, host, query, rows, offset = 0, total = rows.length) {
  const request = navigationScopedRequest(revision, offset, bytes(query), scope, host);
  const records = rows.flatMap(row => {
    const target = rowTarget(row);
    const label = bytes(row.label);
    return [row.index % 256, Math.floor(row.index / 256), label.length, target.length % 256, Math.floor(target.length / 256), ...target, ...label];
  });
  const metadata = rows.flatMap(row => [row.kind, Number(row.selectable), bytes(row.detail).length, ...bytes(row.detail)]);
  return new Uint8Array([...request, total % 256, Math.floor(total / 256), rows.length, ...records, 0x4e, ...metadata]);
}
const metadataStart = encoded => encoded.length - 1 - (3 + bytes(detail).length);
const row = (index, kind, host, selectable = true, identity = index) => ({ index, kind, host, selectable, identity, label: 'Build', detail });
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
function open() {
  return step({ ...initialModel()[0], engineRevision: revision, engineConnected: true }, { kind: 'palette_open' })[0];
}
function committedCommand(cmd) {
  assert.equal(cmd.op, 'batch');
  assert.equal(cmd.cmds.length, 2);
  assert.deepEqual(cmd.cmds[0], { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  return cmd.cmds[1];
}

test('host activation filters by exact identity and stale scope replies cannot replace results', () => {
  let model = open();
  [model] = step(model, { kind: 'palette_scope', scope: 2 });
  assert.equal(model.paletteLoading, true);
  [model] = step(model, { kind: 'navigation_loaded', body: page(2, empty, '', [row(300, 3, bytes('worker-1'))]) });
  assert.deepEqual(model.paletteRows[0].host, bytes('worker-1'));
  const [filtered, command] = step(model, { kind: 'palette_pick', target: model.paletteRows[0].target });
  assert.equal(filtered.paletteOpen, true);
  assert.equal(filtered.paletteScope, 3);
  assert.deepEqual(command.payload, navigationScopedRequest(revision, 0, empty, 3, bytes('worker-1')));
  assert.equal(command.name, 'cockpit.navigation');
  for (const stale of [page(2, empty, '', [row(300, 3, bytes('worker-1'))]), page(3, bytes('worker-10'), '', [row(2, 0, bytes('worker-10'))])]) {
    assert.equal(step(filtered, { kind: 'navigation_loaded', body: stale })[0], filtered);
  }
  [model] = step(filtered, { kind: 'navigation_loaded', body: page(3, bytes('worker-1'), '', [row(600, 1, bytes('worker-1'), false)]) });
  assert.equal(model.paletteRows[0].selectable, false);
  assert.deepEqual(step(model, { kind: 'palette_submit' }), [model, null]);
  // A painted pick of a current disabled row is refused before the command FIFO.
  assert.deepEqual(step(model, { kind: 'palette_pick', target: model.paletteRows[0].target }), [model, null]);
});

test('a held host filter never becomes catalog authority', () => {
  let model = open();
  [model] = step(model, { kind: 'palette_scope', scope: 2 });
  [model] = step(model, { kind: 'navigation_loaded', body: page(2, empty, '', [row(8, 3, empty)]) });
  const held = model.paletteRows[0].target;
  [model] = step(model, { kind: 'palette_scope', scope: 0 });
  [model] = step(model, { kind: 'navigation_loaded', body: page(0, empty, '', [row(8, 0, empty)]) });
  const [filtered, command] = step(model, { kind: 'palette_pick', target: held });
  assert.equal(command.name, 'cockpit.navigation');
  assert.deepEqual(command.payload, navigationScopedRequest(revision, 0, empty, 3, empty));
  assert.equal(filtered.paletteScope, 3);
  assert.equal(text(filtered.paletteHostLabel), 'Coordinator');
  assert.equal(filtered.tabCommands.queue.length, 0);
});

test('a held painted target keeps its captured identity when a replacement page reuses its index', () => {
  let model = open();
  [model] = step(model, { kind: 'navigation_loaded', body: page(0, empty, '', [row(0, 0, empty, true, 1)]) });
  const held = { kind: 'palette_pick', target: model.paletteRows[0].target };
  const newer = { hi: revision.hi, lo: revision.lo + 1 };
  const replacement = page(0, empty, '', [row(0, 0, empty, true, 2)]);
  new DataView(replacement.buffer).setUint32(2, newer.lo, true);
  [model] = step({ ...model, engineRevision: newer }, { kind: 'navigation_loaded', body: replacement });
  assert.equal(model.paletteRows[0].index, 0);
  // Index reuse cannot retarget: the held action echoes its original identity
  // for native validation, and the current row echoes its replacement.
  const [, heldCommand] = step(model, held);
  assert.deepEqual(committedCommand(heldCommand).payload.subarray(10), catalogTarget(1));
  const [closed, current] = step(model, { kind: 'palette_pick', target: model.paletteRows[0].target });
  assert.equal(closed.paletteOpen, false);
  assert.deepEqual(committedCommand(current).payload.subarray(10), catalogTarget(2));
});

test('navigation scopes echo raw identity independently of query, offset and full revision', () => {
  const host = new Uint8Array(255).fill(0x78);
  host[254] = 0xfe; // identity is opaque, not decoded or visually elided
  const encoded = page(3, host, 'directory', [row(600, 1, host, false)], 4, 5);
  const result = navigationPage(encoded);
  assert.deepEqual(result.revision, revision);
  assert.equal(result.scope, 3);
  assert.equal(result.offset, 4);
  assert.equal(text(result.query), 'directory');
  assert.deepEqual(result.host, host);
  // Terminal rows keep their host only inside the opaque catalog identity.
  assert.deepEqual(result.rows[0].target, catalogTarget(600, host));
  assert.equal(result.rows[0].host.length, 0);
  assert.equal(result.rows[0].index, 600);
  assert.equal(result.rows[0].id, 600);
  assert.equal(result.rows[0].kind, 1);
  assert.equal(result.rows[0].selectable, false);
  assert.equal(result.rows[0].highlighted, true);
  assert.equal(text(result.rows[0].label), 'Build');
  assert.equal(text(result.rows[0].detail), detail);
  assert.deepEqual(navigationIntent(result.revision, result.rows[0].index).slice(10), new Uint8Array([88, 2]));
  const hosts = navigationPage(page(2, empty, '', [row(9, 3, host)]));
  assert.deepEqual(hosts.rows[0].host, host);
  assert.deepEqual(navigationHostFilter(hosts.rows[0].target), host);
});

test('coordinator host is empty and known hosts are distinguishable from terminals', () => {
  const hosts = navigationPage(page(2, empty, '', [row(8, 3, empty), row(20, 3, bytes('worker'))]));
  assert.equal(hosts.scope, 2);
  assert.equal(hosts.rows[0].kind, 3);
  assert.equal(hosts.rows[0].host.length, 0);
  assert.notEqual(navigationHostFilter(hosts.rows[0].target), null);
  const coordinator = navigationPage(page(3, hosts.rows[0].host, '', [row(8, 0, empty)]));
  assert.equal(coordinator.scope, 3);
  assert.equal(coordinator.rows[0].kind, 0);
  assert.equal(navigationHostFilter(coordinator.rows[0].target), null);
});

test('legacy requests and replies retain default scope and row metadata', () => {
  const request = navigationRequest(revision, 0, empty);
  assert.equal(request.length, 13);
  assert.equal(request[1], 3);
  const target = catalogTarget(300);
  const result = navigationPage(new Uint8Array([...request, 1, 0, 1, 44, 1, 1, target.length, 0, ...target, 65]));
  assert.equal(result.scope, 0);
  assert.equal(result.host.length, 0);
  assert.equal(result.rows[0].index, 300);
  assert.deepEqual(result.rows[0].target, target);
  assert.equal(result.rows[0].selectable, true);
  assert.equal(result.rows[0].detail.length, 0);
});

test('scoped framing refuses truncation, malformed metadata and identity overflow', () => {
  const encoded = page(3, bytes('worker'), 'q', [row(900, 1, bytes('worker'))]);
  // Scoped replies require metadata; legacy kind-3 replies may omit it.
  const marker = metadataStart(encoded);
  assert.equal(encoded[marker], 0x4e);
  assert.equal(navigationPage(encoded.slice(0, marker)), null);
  for (let end = marker + 1; end < encoded.length; end++) assert.equal(navigationPage(encoded.slice(0, end)), null, `length ${end}`);
  for (const [at, value] of [[marker + 1, 4], [marker + 2, 4], [marker + 3, 161]]) {
    const bad = encoded.slice(); bad[at] = value;
    assert.equal(navigationPage(bad), null);
  }
  assert.equal(navigationPage(new Uint8Array([...encoded, 0])), null);
  // Kind and target must agree: a host kind cannot label catalog authority,
  // and a filter token cannot masquerade as a terminal row.
  const hostKind = encoded.slice(); hostKind[marker + 1] = 3;
  assert.equal(navigationPage(hostKind), null);
  const hosts = page(2, empty, '', [row(8, 3, bytes('worker'))]);
  const terminalKind = hosts.slice(); terminalKind[metadataStart(hosts) + 1] = 1;
  assert.equal(navigationPage(terminalKind), null);
  // Request echo (15) + total/count (3) + index/label/target lengths (5).
  const tokenAt = 23;
  assert.deepEqual(hosts.subarray(tokenAt, tokenAt + 8), hostToken(bytes('worker')));
  for (const [at, value] of [[tokenAt, 4], [tokenAt + 1, 5]]) {
    const bad = hosts.slice(); bad[at] = value;
    assert.equal(navigationPage(bad), null);
  }
  assert.equal(navigationScopedRequest(revision, 0, empty, 3, new Uint8Array(256)).length, 0);
  assert.equal(navigationScopedRequest(revision, 0, empty, 2, bytes('host')).length, 0);
  assert.equal(navigationScopedRequest(revision, 0, new Uint8Array(65), 0, empty).length, 0);
  assert.equal(navigationScopedRequest(revision, 65536, empty, 0, empty).length, 0);
  assert.equal(navigationScopedRequest(revision, 0, empty, 5, empty).length, 0);
  const noMatches = navigationPage(page(1, empty, 'absent', []));
  assert.equal(noMatches.scope, 1);
  assert.equal(noMatches.total, 0);
  assert.equal(noMatches.rows.length, 0);
});

function bareSnapshot() {
  const out = new Uint8Array(33);
  out[0] = 1; out[1] = 2; out[23] = 2; out[26] = 168; out[29] = 255;
  return out;
}
function contextRecord(fields) {
  const payload = fields.flatMap(value => [bytes(value).length, ...bytes(value)]);
  return [3, payload.length % 256, Math.floor(payload.length / 256), ...payload];
}
test('snapshot navigation context is additive, bounded and defaults empty without extensions', () => {
  const legacy = snapshot(bareSnapshot());
  for (const key of ['currentSession', 'coordinatorEndpoint', 'connectionDetail']) assert.equal(legacy[key].length, 0);
  const record = contextRecord(['selected-session', '/real/coordinator.sock', 'Connected to coordinator']);
  const encoded = new Uint8Array([...bareSnapshot(), 0, 0, 0, 0, 0, ...record]);
  const result = snapshot(encoded);
  assert.equal(text(result.currentSession), 'selected-session');
  assert.equal(text(result.coordinatorEndpoint), '/real/coordinator.sock');
  assert.equal(text(result.connectionDetail), 'Connected to coordinator');
  for (let end = 39; end < encoded.length; end++) assert.equal(snapshot(encoded.slice(0, end)), null);
  const oversized = contextRecord(['s'.repeat(65), '', '']);
  assert.equal(snapshot(new Uint8Array([...bareSnapshot(), 0, 0, 0, 0, 0, ...oversized])), null);
});
