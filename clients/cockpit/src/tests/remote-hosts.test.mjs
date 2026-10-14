import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update, commandMsg, windows } from '../core.ts';
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
function snapshotBytes(connection, activeWindow = 0) {
  const out = new Uint8Array(activeWindow === 0 ? 33 : 40);
  out[0] = 1; out[1] = 2; out[10] = 7; out[18] = activeWindow; out[23] = connection;
  out[26] = 168; out[29] = 255;
  if (activeWindow > 0) {
    out[32] = 1;
    out.set([activeWindow, 0, 0, 0, 0, 168, 0], 33);
  }
  return out;
}
function assertRemoteRequest(cmd, payload) {
  assert.equal(cmd.op, 'request');
  assert.equal(cmd.name, 'cockpit.remote');
  assert.deepEqual(cmd.payload, payload);
}

test('Machines is a menu command with its own chord, not cmd+shift+G', () => {
  assert.deepEqual(commandMsg('navigator.machines'), { kind: 'machines_open' });
  const zon = readFileSync(new URL('../../app.zon', import.meta.url), 'utf8');
  assert.match(zon, /\.label = "Machines…", \.command = "navigator\.machines", \.key = "o", \.modifiers = \.\{ "primary", "shift" \}/);
  assert.match(zon, /\.id = "navigator\.machines", \.key = "o", \.modifiers = \.\{ "primary", "shift" \}/);
  const shiftG = zon.split('\n').filter(line => /\.key = "g", \.modifiers = \.\{ "primary", "shift" \}/.test(line));
  assert.ok(shiftG.length > 0);
  for (const line of shiftG) assert.match(line, /terminal\.find-previous/);
});

test('Machines is the one place to connect: no Connect to Host flow remains', () => {
  assert.equal(commandMsg('remote.connect'), null);
  const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
  const sheet = read('../windows/components/cockpit-host.native');
  assert.doesNotMatch(sheet, /Connect to Host|host_local|host_disconnect|Use this Mac/);
  const location = read('../windows/components/cockpit-connected-hosts.native');
  assert.doesNotMatch(location, /Connect to Host|header_host_pick|Refresh hosts|Show more hosts/);
  assert.match(location, /on-press="header_machines">Machines…<\/menu-item>/);
});

test('the machine sheet opens in whichever window invoked it, not always the main one', () => {
  const secondary = snapshotBytes(2, 1);
  let [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body: secondary });
  assert.equal(model.activeWindow, 1);
  assert.equal(windows(model).length, 1);
  assert.equal(text(windows(model)[0].label), 'phux-window-1');
  [model] = step(model, { kind: 'add_machine_open' });
  assert.equal(model.hostOpen, true);
  assert.equal(model.mainHostOpen, false);
  assert.equal(model.window1HostOpen, true);
  assert.equal(model.window2HostOpen, false);
  [model] = step(model, { kind: 'host_close' });
  assert.equal(model.window1HostOpen, false);

  // Every window's markup presents the shared sheet through its own flag. The
  // sheet is its own template: another cockpit-window argument would push the
  // tab loop past the live interpreter's 16-entry scope bound.
  const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
  const component = read('../windows/components/cockpit-host.native');
  assert.match(component, /<template name="cockpit-host" args="hostopen">/);
  assert.doesNotMatch(read('../windows/components/cockpit-window.native'), /<template name="cockpit-window" args="[^"]*hostopen/);
  const use = flag => new RegExp(`<use template="cockpit-host" hostopen="\\{${flag}\\}" />`);
  assert.match(read('../app.native'), use('mainHostOpen'));
  for (const n of [1, 2, 3, 4]) assert.match(read(`../windows/phux-window-${n}.native`), use(`window${n}HostOpen`));
});

test('a connected registered host says nothing in header status; trouble names the host', () => {
  let model = initialModel()[0];
  [model] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  [model] = step(model, { kind: 'remote_loaded', body: reply(2, 'mini') });
  assert.equal(text(model.connectionStatus), '');
  [model] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assert.equal(text(model.connectionStatus), '');
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

test('the codec refuses what it cannot frame or read', () => {
  assert.deepEqual(remoteRequest(1, new Uint8Array(300)), new Uint8Array([1, 1, 0]));
  assert.equal(remoteReply(new Uint8Array([2, 1, 0, 0])), null, 'version');
  assert.equal(remoteReply(new Uint8Array([1, 6, 0, 0])), null, 'phase');
  assert.equal(remoteReply(new Uint8Array([1, 5, 0, 0])).phase, 5, 'refused');
  assert.equal(remoteReply(new Uint8Array([1, 1, 4, 0])), null, 'host overruns');
  assert.equal(remoteReply(new Uint8Array([1, 1, 0, 0, 9])), null, 'trailing bytes');
  const parsed = remoteReply(reply(3, 'mini', 'why'));
  assert.equal(text(parsed.host), 'mini');
  assert.equal(text(parsed.reason), 'why');
  assert.equal(remoteStatusLine(remoteReply(reply(0, ''))).length, 0);
});
