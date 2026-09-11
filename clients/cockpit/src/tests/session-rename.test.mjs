import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update, commandMsg } from '../core.ts';
import { sessionRequest, sessionReply } from '../session.ts';

const bytes = value => new TextEncoder().encode(value);
const text = value => new TextDecoder().decode(value);
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
function reply(phase, name, host, reason = '') {
  const n = bytes(name);
  const h = bytes(host);
  const r = bytes(reason);
  return new Uint8Array([1, phase, n.length, ...n, h.length, ...h, r.length, ...r]);
}
function snapshotBytes(connection) {
  const out = new Uint8Array(33);
  out[0] = 1; out[1] = 2; out[10] = 7; out[23] = connection;
  out[26] = 168; out[29] = 255;
  return out;
}
function sessionRequests(cmd) {
  if (cmd === null) return [];
  const all = cmd.op === 'batch' ? cmd.cmds : [cmd];
  return all.filter(one => one.op === 'request' && one.name === 'cockpit.session').map(one => one.payload);
}
function opened(name = 'fixture', host = 'mini') {
  const [model] = step(initialModel()[0], { kind: 'rename_open' });
  return step(model, { kind: 'session_loaded', body: reply(0, name, host) })[0];
}

test('Rename Session is a Window menu command', () => {
  assert.deepEqual(commandMsg('session.rename'), { kind: 'rename_open' });
  const zon = readFileSync(new URL('../../app.zon', import.meta.url), 'utf8');
  assert.match(zon, /\.label = "Rename Session…", \.command = "session\.rename"/);
  const markup = readFileSync(new URL('../windows/components/cockpit-window.native', import.meta.url), 'utf8');
  assert.match(markup, /<template name="cockpit-rename" args="renameopen">/);
  for (const file of ['../app.native', '../windows/phux-window-1.native', '../windows/phux-window-4.native']) {
    assert.match(readFileSync(new URL(file, import.meta.url), 'utf8'), /<use template="cockpit-rename" renameopen="\{\w+RenameOpen\}" \/>/);
  }
});

test('opening asks which session is on screen, takes the modal slot, and seeds the field with its name', () => {
  let [model, cmd] = step(initialModel()[0], { kind: 'palette_open' });
  [model, cmd] = step(model, { kind: 'rename_open' });
  assert.equal(model.renameOpen, true);
  assert.equal(model.mainRenameOpen, true);
  assert.equal(model.paletteOpen, false);
  assert.equal(cmd.op, 'batch');
  assert.deepEqual(cmd.cmds[0], { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 1, 0])]);
  [model] = step(model, { kind: 'session_loaded', body: reply(0, 'fixture', 'mini') });
  assert.equal(text(model.renameQuery), 'fixture');
  assert.equal(text(model.renameTitle), 'Rename fixture on mini');
  assert.equal(model.renameBusy, false);
  // Another modal takes the slot back.
  [model] = step(model, { kind: 'palette_open' });
  assert.equal(model.renameOpen, false);
  assert.equal(model.mainRenameOpen, false);
});

test('nothing on screen to rename says so and sends nothing more', () => {
  let [model] = step(initialModel()[0], { kind: 'rename_open' });
  [model] = step(model, { kind: 'session_loaded', body: reply(4, '', '', 'No Phux session is on screen to rename.') });
  assert.equal(model.renameOpen, true);
  assert.equal(text(model.renameNotice), 'No Phux session is on screen to rename.');
});

test('submitting sends the new name; a refused rename keeps the panel and shows the reason', () => {
  let model = { ...opened(), renameQuery: bytes('deploy') };
  let cmd;
  [model, cmd] = step(model, { kind: 'rename_submit' });
  assert.deepEqual(sessionRequests(cmd), [sessionRequest(2, bytes('deploy'))]);
  assert.equal(model.renameBusy, true);
  // A second Enter while it is out sends nothing.
  assert.deepEqual(sessionRequests(step(model, { kind: 'rename_submit' })[1]), []);
  [model] = step(model, { kind: 'session_loaded', body: reply(3, 'fixture', 'mini', '"deploy" already exists on mini.') });
  assert.equal(model.renameOpen, true);
  assert.equal(model.renameBusy, false);
  assert.equal(text(model.renameNotice), '"deploy" already exists on mini.');
  assert.equal(text(model.renameQuery), 'deploy', 'the typed name stays for another try');
  // An empty name is refused here.
  [model, cmd] = step({ ...model, renameQuery: new Uint8Array() }, { kind: 'rename_submit' });
  assert.deepEqual(sessionRequests(cmd), []);
  assert.equal(text(model.renameNotice), 'Enter a new name for this session.');
});

test('a pending rename asks for its outcome on each snapshot, and a confirmed one closes the panel', () => {
  let model = { ...opened(), renameQuery: bytes('ship') };
  let cmd;
  [model] = step(model, { kind: 'rename_submit' });
  [model] = step(model, { kind: 'session_loaded', body: reply(1, 'fixture', 'mini') });
  assert.equal(model.renameAwaiting, true);
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 3, 0])]);
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 3, 0])], 'polled again while pending');
  [model, cmd] = step(model, { kind: 'session_loaded', body: reply(2, 'ship', 'mini') });
  assert.equal(model.renameOpen, false);
  assert.deepEqual(cmd, { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  // Settled: snapshots no longer ask.
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assert.deepEqual(sessionRequests(cmd), []);
});

test('Escape closes the panel and gives the keyboard back', () => {
  let [model, cmd] = step(opened(), { kind: 'palette_close' });
  assert.equal(model.renameOpen, false);
  assert.deepEqual(cmd, { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  // A late answer after Escape changes nothing.
  [model] = step(model, { kind: 'session_loaded', body: reply(3, 'fixture', 'mini', 'late') });
  assert.equal(model.renameOpen, false);
});

/// A captured catalog target (catalog_targets.zig): tag 2, `resource` 2 for
/// the active coordinator's session, 3 for a peer's, 1 for a terminal.
function rowTarget(resource, session, provider = 0x80000001) {
  const out = new Uint8Array(resource === 1 ? 43 : 38);
  out[0] = 2;
  out[1] = resource;
  const view = new DataView(out.buffer);
  view.setBigUint64(2, BigInt(provider), true);
  view.setUint32(34, session, true);
  return out;
}

test('a session row offers Rename in its context menu, by its own captured target', () => {
  const markup = readFileSync(new URL('../windows/components/cockpit-window.native', import.meta.url), 'utf8');
  assert.match(markup, /on-press="palette_pick:\{row\.target\}">[\s\S]*?<context-menu>\s*<if test="\{row\.renamable\}">\s*<menu-item on-press="rename_row:\{row\.target\}">Rename Session…<\/menu-item>/);
});

test('only a listed session row is renamable: never a terminal or a peer group row', async () => {
  const { navigationScopedRequest } = await import('../protocol.ts');
  const revision = { hi: 0, lo: 7 };
  const rows = [
    ['mini build', rowTarget(3, 1), 2, 1],
    ['This Mac fixture', rowTarget(2, 1, 1), 2, 1],
    ['mini unavailable', rowTarget(3, 0), 2, 1],
    ['a terminal', rowTarget(1, 7), 0, 1],
  ];
  const head = navigationScopedRequest(revision, 0, new Uint8Array(), 1, new Uint8Array());
  const records = rows.flatMap(([label, target], index) => {
    const l = bytes(label);
    return [index, 0, l.length, target.length, 0, ...target, ...l];
  });
  const metadata = rows.flatMap(([, , kind, selectable]) => [kind, selectable, 0]);
  const body = new Uint8Array([...head, rows.length, 0, rows.length, ...records, 0x4e, ...metadata]);
  let [model] = step({ ...initialModel()[0], engineRevision: revision, engineConnected: true }, { kind: 'palette_open' });
  [model] = step(model, { kind: 'palette_scope', scope: 1 });
  [model] = step(model, { kind: 'navigation_loaded', body });
  assert.deepEqual(model.paletteRows.map(row => row.renamable), [true, true, false, false]);
});

test('Rename on a row describes and renames that row, never the session on screen', () => {
  const target = rowTarget(3, 1);
  let [model, cmd] = step(initialModel()[0], { kind: 'palette_open' });
  [model, cmd] = step(model, { kind: 'rename_row', target });
  assert.equal(model.renameOpen, true);
  assert.equal(model.paletteOpen, false, 'the panel takes the modal slot');
  assert.deepEqual(cmd.cmds[0], { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 6, 0, 38, ...target])]);
  [model] = step(model, { kind: 'session_loaded', body: reply(0, 'build', 'mini') });
  assert.equal(text(model.renameTitle), 'Rename build on mini');
  model = { ...model, renameQuery: bytes('ship') };
  [model, cmd] = step(model, { kind: 'rename_submit' });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 7, 4, ...bytes('ship'), 38, ...target])]);
  // The coordinator's refusal keeps the panel and says why.
  [model] = step(model, { kind: 'session_loaded', body: reply(3, 'build', 'mini', '"ship" already exists on mini.') });
  assert.equal(model.renameOpen, true);
  assert.equal(text(model.renameNotice), '"ship" already exists on mini.');
  // A stale row is refused by the engine; the panel shows its reason.
  [model] = step(model, { kind: 'session_loaded', body: reply(4, '', '', 'That session is no longer listed there.') });
  assert.equal(text(model.renameNotice), 'That session is no longer listed there.');

  // Window > Rename Session afterwards names the session on screen again.
  [model] = step(model, { kind: 'rename_close' });
  [model, cmd] = step(model, { kind: 'rename_open' });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 1, 0])]);
  [model, cmd] = step({ ...model, renameBusy: false, renameQuery: bytes('x') }, { kind: 'rename_submit' });
  assert.deepEqual(sessionRequests(cmd), [sessionRequest(2, bytes('x'))]);
});

test('Rename on a row that is not a session row opens nothing and sends nothing', () => {
  const [opened] = step(initialModel()[0], { kind: 'palette_open' });
  for (const target of [rowTarget(1, 7), rowTarget(3, 0), new Uint8Array()]) {
    const [model, cmd] = step(opened, { kind: 'rename_row', target });
    assert.equal(model.renameOpen, false);
    assert.equal(model.paletteOpen, true);
    assert.deepEqual(sessionRequests(cmd), []);
  }
});

test('the reply codec reads each field and refuses torn replies', () => {
  const decoded = sessionReply(reply(3, 'a', 'mini', 'why'));
  assert.equal(decoded.phase, 3);
  assert.equal(text(decoded.name), 'a');
  assert.equal(text(decoded.host), 'mini');
  assert.equal(text(decoded.reason), 'why');
  assert.equal(sessionReply(new Uint8Array([1, 2, 0, 0])), null);
  assert.equal(sessionReply(new Uint8Array([1, 9, 0, 0, 0])), null);
  assert.equal(sessionReply(new Uint8Array([2, 0, 0, 0, 0])), null);
  assert.deepEqual(sessionRequest(2, new Uint8Array(300)), new Uint8Array([1, 2, 0]));
});
