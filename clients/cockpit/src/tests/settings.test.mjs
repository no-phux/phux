import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialAppearance, appearanceResponse } from '../appearance.ts';
import { settingsCatalog, settingsRows, settingRequest, resetSettingRequest } from '../settings.ts';

const bytes = value => new TextEncoder().encode(value);
const text = value => new TextDecoder().decode(value);

test('settings search finds meaningful ownership across groups', () => {
  const rows = settingsRows(initialAppearance(), bytes('scratch'), 0);
  assert.deepEqual(rows.map(row => row.id), [3, 4, 5, 6, 7]);
  assert.equal(settingsRows(initialAppearance(), bytes('EDITOR'), 0)[0].id, 10);
  assert.equal(settingsRows(initialAppearance(), bytes('no such setting'), 0).length, 0);
  assert.deepEqual(settingsRows(initialAppearance(), bytes(''), 3).map(row => row.id), [9]);
});

test('catalog names actual timing, defaults, and remote owner route', () => {
  const rows = settingsCatalog();
  for (const row of rows) {
    assert.ok(row.defaultLabel.length > 0);
    assert.ok(row.applicability.length > 0);
    assert.ok(row.timing.length > 0);
  }
  assert.match(text(rows[3].applicability), /Scratch.*Phux/);
  assert.match(text(rows[6].timing), /New scratch/);
  assert.equal(rows[12].editable, false);
  assert.match(text(rows[13].applicability), /phux config path/);
});

test('unsupported requested font names do not masquerade as effective faces', () => {
  const appearance = { ...initialAppearance(), values: [bytes('Unknown Font')] };
  const row = settingsRows(appearance, bytes('font family'), 0)[0];
  assert.equal(text(row.value), 'Unknown Font');
  assert.match(text(row.effectiveValue), /Bundled.*unsupported/);
});

function reply() {
  const values = ['Menlo', '15', 'auto', '3', 'bar', 'false', '52428800', '', 'true', 'side', 'nvim -f'];
  const records = values.flatMap((value, id) => {
    const b = bytes(value);
    return [id, b.length, 0, ...b];
  });
  return new Uint8Array([2, 1, 0, 1, 0, 1, 1, 0, 0, 0, 1, ...records]);
}

test('settings response round trips values and rejects truncated or reordered records', () => {
  const wire = reply();
  const appearance = appearanceResponse(wire);
  assert.equal(appearance.followSystem, true);
  assert.equal(text(appearance.values[10]), 'nvim -f');
  const rows = settingsRows(appearance, bytes('editor'), 0);
  assert.equal(text(rows[0].value), 'nvim -f');
  for (let size = 0; size < wire.length; size++) assert.equal(appearanceResponse(wire.slice(0, size)), null);
  const wrong = wire.slice(); wrong[11] = 2;
  assert.equal(appearanceResponse(wrong), null);
});

test('editor requests preserve Unicode arguments without a shell string interpolation', () => {
  const value = bytes('"/Applications/My Editor/bin/edit" --wait café');
  assert.deepEqual(settingRequest(10, value), new Uint8Array([2, 8, 10, ...value]));
  assert.deepEqual([...resetSettingRequest(10)], [2, 9, 10]);
});

test('external conflict and malformed reload retain explicit recovery messages', () => {
  const wire = reply();
  wire[2] = 6;
  assert.match(text(appearanceResponse(wire).notice), /outside Settings/);
  wire[2] = 7;
  assert.match(text(appearanceResponse(wire).notice), /last-good/);
});
