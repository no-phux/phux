import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update, commandMsg } from '../core.ts';
import { selfUpdateRequest, selfUpdateResponse } from '../self-update.ts';

const text = value => new TextDecoder().decode(value);
const bytes = value => new TextEncoder().encode(value);

function field(value) {
  const body = bytes(value);
  return [body.length & 255, (body.length >> 8) & 255, ...body];
}

function reply(status, flags, current, latest, message, remedy) {
  return new Uint8Array([1, status, flags, ...field(current), ...field(latest), ...field(message), ...field(remedy)]);
}

const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};

test('self-update response is whole or rejected', () => {
  const wire = reply(1, 1, '0.24.0', 'cockpit-v0.25.0', 'newer available', '');
  const parsed = selfUpdateResponse(wire);
  assert.equal(parsed.status, 1);
  assert.equal(parsed.canInstall, true);
  assert.equal(text(parsed.latest), 'cockpit-v0.25.0');
  assert.equal(selfUpdateResponse(wire.slice(0, wire.length - 1)), null);
  assert.deepEqual(selfUpdateRequest(false), new Uint8Array([1, 0]));
  assert.deepEqual(selfUpdateRequest(true), new Uint8Array([1, 1]));
});

test('Check for Updates opens About and requests the installer driver', () => {
  const [model, cmd] = step(initialModel()[0], commandMsg('app.update'));
  assert.equal(model.settingsOpen, true);
  assert.equal(model.settingsSection, 5);
  assert.equal(model.updateBusy, true);
  const names = cmd.cmds.map(effect => effect.name);
  assert.ok(names.includes('cockpit.appearance'));
  assert.ok(names.includes('cockpit.update'));
  assert.deepEqual([...cmd.cmds.find(effect => effect.name === 'cockpit.update').payload], [1, 0]);
});

test('current, newer, refused and failed replies stay visible', () => {
  let [model] = step(initialModel()[0], commandMsg('app.update'));
  [model] = step(model, { kind: 'update_loaded', body: reply(0, 0, '0.24.0', 'cockpit-v0.24.0', 'Phux Cockpit 0.24.0 is current.', '') });
  assert.equal(model.updateBusy, false);
  assert.equal(model.updateCanInstall, false);
  assert.match(text(model.updateStatus), /current/);
  [model] = step(model, { kind: 'update_loaded', body: reply(1, 1, '0.24.0', 'cockpit-v0.25.0', 'Phux Cockpit 0.25.0 is available.', '') });
  assert.equal(model.updateCanInstall, true);
  const [, install] = step(model, { kind: 'update_install' });
  assert.equal(install.name, 'cockpit.update');
  assert.deepEqual([...install.payload], [1, 1]);
  [model] = step(model, { kind: 'update_loaded', body: reply(2, 0, '0.24.0', '', 'This copy was installed with Homebrew.', 'brew upgrade --cask no-phux/tap/phux-cockpit') });
  assert.equal(model.updateCanInstall, false);
  assert.match(text(model.updateRemedy), /brew upgrade --cask/);
  [model] = step(model, { kind: 'update_failed', error: bytes('checksum mismatch') });
  assert.match(text(model.updateStatus), /checksum mismatch/);
});
