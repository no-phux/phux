import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update, commandMsg } from '../core.ts';
import { remoteRequest, remoteReply, remoteStatusLine } from '../remote-hosts.ts';

const bytes = value => new TextEncoder().encode(value);
const text = value => new TextDecoder().decode(value);
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
function reply(phase, host, reason = '') {
  const h = bytes(host);
  const r = bytes(reason);
  return new Uint8Array([1, phase, h.length, ...h, r.length, ...r]);
}
function snapshotBytes(connection) {
  const out = new Uint8Array(33);
  out[0] = 1; out[1] = 2; out[10] = 7; out[23] = connection;
  out[26] = 168; out[29] = 255;
  return out;
}
function assertRemoteRequest(cmd, payload) {
  assert.equal(cmd.op, 'request');
  assert.equal(cmd.name, 'cockpit.remote');
  assert.deepEqual(cmd.payload, payload);
}
function opened() {
  return step(initialModel()[0], { kind: 'host_open' })[0];
}

test('Connect to Host is a menu command with its own chord, not cmd+shift+G', () => {
  assert.deepEqual(commandMsg('remote.connect'), { kind: 'host_open' });
  const zon = readFileSync(new URL('../../app.zon', import.meta.url), 'utf8');
  assert.match(zon, /\.label = "Connect to Host…", \.command = "remote\.connect", \.key = "o", \.modifiers = \.\{ "primary", "shift" \}/);
  assert.match(zon, /\.id = "remote\.connect", \.key = "o", \.modifiers = \.\{ "primary", "shift" \}/);
  const shiftG = zon.split('\n').filter(line => /\.key = "g", \.modifiers = \.\{ "primary", "shift" \}/.test(line));
  assert.ok(shiftG.length > 0);
  for (const line of shiftG) assert.match(line, /terminal\.find-previous/);
});

test('opening asks for status and takes the modal slot from the switcher', () => {
  let [model, cmd] = step(initialModel()[0], { kind: 'palette_open' });
  [model, cmd] = step(model, { kind: 'host_open' });
  assert.equal(model.hostOpen, true);
  assert.equal(model.mainHostOpen, true);
  assert.equal(model.paletteOpen, false);
  assert.equal(cmd.op, 'batch');
  assert.deepEqual(cmd.cmds[0], { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  assertRemoteRequest(cmd.cmds[1], new Uint8Array([1, 1, 0]));
  [model] = step(model, { kind: 'palette_open' });
  assert.equal(model.hostOpen, false);
  assert.equal(model.mainHostOpen, false);
});

test('submit sends the typed host; a failure keeps it and the panel for a retry', () => {
  let model = { ...opened(), hostQuery: bytes('me@mini') };
  let cmd;
  [model, cmd] = step(model, { kind: 'host_submit' });
  assertRemoteRequest(cmd, new Uint8Array([1, 2, 7, ...bytes('me@mini')]));
  assert.equal(model.hostBusy, true);
  assert.equal(step(model, { kind: 'host_submit' })[1], null, 'one connect in flight');

  const reason = 'me@mini is not a registered host; pair it once in a terminal with `phux --remote me@mini`';
  [model, cmd] = step(model, { kind: 'remote_loaded', body: reply(3, 'me@mini', reason) });
  assert.equal(cmd, null);
  assert.equal(model.hostOpen, true);
  assert.equal(model.hostBusy, false);
  assert.equal(text(model.hostQuery), 'me@mini');
  assert.equal(text(model.hostNotice), `Could not connect to me@mini: ${reason}`);
  assert.equal(text(model.connectionStatus), `Could not connect to me@mini: ${reason}`);

  [, cmd] = step(model, { kind: 'host_submit' });
  assertRemoteRequest(cmd, new Uint8Array([1, 2, 7, ...bytes('me@mini')]));
});

test('an empty host is prompted for instead of sent', () => {
  const [model, cmd] = step(opened(), { kind: 'host_submit' });
  assert.equal(cmd, null);
  assert.match(text(model.hostNotice), /registered host/);
});

test('connecting, then connected, closes the panel and names the host in the status bar', () => {
  let model = { ...opened(), hostQuery: bytes('mini') };
  let cmd;
  [model] = step(model, { kind: 'host_submit' });
  [model, cmd] = step(model, { kind: 'remote_loaded', body: reply(1, 'mini') });
  assert.equal(cmd, null);
  assert.equal(model.hostOpen, true);
  assert.equal(text(model.hostNotice), 'Connecting to mini...');

  // Still awaiting: the next snapshot asks again even without a change.
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assertRemoteRequest(cmd, new Uint8Array([1, 1, 0]));
  [model, cmd] = step(model, { kind: 'remote_loaded', body: reply(2, 'mini') });
  assert.equal(model.hostOpen, false);
  assert.deepEqual(cmd, { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  assert.equal(text(model.connectionStatus), 'Connected to mini');

  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assert.equal(cmd, null, 'no status request while nothing moved');
  assert.equal(text(model.connectionStatus), 'mini / Phux connected');
});

test('reconnecting and a lost connection are named for the host', () => {
  let model = initialModel()[0];
  [model] = step(model, { kind: 'remote_loaded', body: reply(4, 'mini') });
  assert.equal(text(model.connectionStatus), 'Reconnecting to mini...');
  [model] = step(model, { kind: 'remote_loaded', body: reply(3, 'mini', 'the connection was lost') });
  [model] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(3) });
  assert.equal(text(model.connectionStatus), 'Could not connect to mini: the connection was lost');
  assert.equal(model.canReconnect, true);
});

test('status is asked for when the connection moves, not once per snapshot', () => {
  let model = initialModel()[0];
  let cmd;
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assertRemoteRequest(cmd, new Uint8Array([1, 1, 0]));
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assert.equal(cmd, null);
  [, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(1) });
  assertRemoteRequest(cmd, new Uint8Array([1, 1, 0]));
});

test('Use this Mac returns to the local coordinator and closes the panel', () => {
  let [model, cmd] = step(opened(), { kind: 'host_local' });
  assertRemoteRequest(cmd, new Uint8Array([1, 3, 0]));
  [model, cmd] = step(model, { kind: 'remote_loaded', body: reply(0, '') });
  assert.equal(model.hostOpen, false);
  assert.equal(cmd.name, 'cockpit.committed');
  assert.equal(model.hostName.length, 0);
});

test('the codec refuses what it cannot frame or read', () => {
  assert.deepEqual(remoteRequest(2, new Uint8Array(300)), new Uint8Array([1, 2, 0]));
  assert.equal(remoteReply(new Uint8Array([2, 1, 0, 0])), null, 'version');
  assert.equal(remoteReply(new Uint8Array([1, 5, 0, 0])), null, 'phase');
  assert.equal(remoteReply(new Uint8Array([1, 1, 4, 0])), null, 'host overruns');
  assert.equal(remoteReply(new Uint8Array([1, 1, 0, 0, 9])), null, 'trailing bytes');
  const parsed = remoteReply(reply(3, 'mini', 'why'));
  assert.equal(text(parsed.host), 'mini');
  assert.equal(text(parsed.reason), 'why');
  assert.equal(remoteStatusLine(remoteReply(reply(0, ''))).length, 0);
});
