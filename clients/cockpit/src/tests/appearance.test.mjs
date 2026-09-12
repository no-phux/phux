import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';
import { appearanceResponse } from '../appearance.ts';

const text = value => new TextDecoder().decode(value);
const bytes = value => new TextEncoder().encode(value);
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
function reply({ active = true, outcome = 0, dirty = false, theme = 0, cursor = 0, placement = 0 } = {}) {
  const font = bytes('14.0 pt');
  const contrast = bytes('15.2:1 text contrast');
  return new Uint8Array([1, +active, outcome, +dirty, theme, cursor, placement, 0, font.length, contrast.length, ...font, ...contrast]);
}
function opened() {
  const model = step(initialModel()[0], { kind: 'settings_open' })[0];
  return step(model, { kind: 'appearance_loaded', body: reply() })[0];
}

test('appearance response rejects malformed lengths and flags', () => {
  const valid = reply();
  assert.equal(text(appearanceResponse(valid).fontLabel), '14.0 pt');
  for (let i = 0; i < valid.length; ++i) assert.equal(appearanceResponse(valid.slice(0, i)), null);
  for (const at of [1, 2, 3, 5, 6, 7]) {
    const malformed = valid.slice(); malformed[at] = 255;
    assert.equal(appearanceResponse(malformed), null);
  }
});

test('opening admits a transaction and blocks preview changes until its reply', () => {
  const [model, cmd] = step(initialModel()[0], { kind: 'settings_open' });
  assert.equal(model.mainSettingsOpen, true);
  assert.equal(model.appearanceBusy, true);
  assert.equal(cmd.cmds[0].name, 'cockpit.committed');
  assert.equal(cmd.cmds.at(-1).name, 'cockpit.appearance');
  assert.deepEqual([...cmd.cmds.at(-1).payload], [1, 0, 0]);
  assert.deepEqual(step(model, { kind: 'settings_font', direction: 1 }), [model, null]);
});

test('Save stays open until confirmed; a refused write preserves Cancel', () => {
  let [model, cmd] = step(opened(), { kind: 'settings_commit' });
  assert.equal(model.settingsOpen, true);
  assert.deepEqual([...cmd.payload], [1, 7, 0]);
  [model] = step(model, { kind: 'appearance_loaded', body: reply({ outcome: 3, dirty: true }) });
  assert.equal(model.settingsOpen, true);
  assert.match(text(model.appearance.notice), /Could not save/);
  [model, cmd] = step(model, { kind: 'settings_close' });
  assert.deepEqual([...cmd.payload], [1, 6, 0]);
  assert.equal(model.settingsOpen, true);
  [model, cmd] = step(model, { kind: 'appearance_loaded', body: reply({ active: false, outcome: 2 }) });
  assert.equal(model.settingsOpen, false);
  assert.equal(cmd.name, 'cockpit.committed');
});

test('opening navigation from a preview waits for rollback before showing results', () => {
  let [model, cmd] = step(opened(), { kind: 'palette_open' });
  assert.equal(model.paletteOpen, false);
  assert.equal(model.navigationAfterSettings, true);
  assert.deepEqual([...cmd.payload], [1, 6, 0]);
  [model, cmd] = step(model, { kind: 'appearance_loaded', body: reply({ active: false, outcome: 2 }) });
  assert.equal(model.settingsOpen, false);
  assert.equal(model.paletteOpen, true);
  assert.equal(cmd.cmds[0].name, 'cockpit.committed');
});

test('a failed rollback still dismisses Settings and releases the keyboard', () => {
  let [model, cmd] = step(opened(), { kind: 'settings_close' });
  assert.deepEqual([...cmd.payload], [1, 6, 0]);
  [model, cmd] = step(model, { kind: 'appearance_failed', error: bytes('engine unavailable') });
  assert.equal(model.settingsOpen, false);
  assert.equal(model.mainSettingsOpen, false);
  assert.equal(model.appearanceBusy, false);
  assert.deepEqual(cmd, { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  // A failed rollback on the way to the switcher still reaches the switcher.
  [model] = step(opened(), { kind: 'palette_open' });
  [model, cmd] = step(model, { kind: 'appearance_failed', error: bytes('engine unavailable') });
  assert.equal(model.settingsOpen, false);
  assert.equal(model.paletteOpen, true);
  assert.ok(cmd.cmds.some(effect => effect.name === 'cockpit.navigation'));
});

test('without a native transaction Cancel closes locally, while a failed Save keeps the preview', () => {
  let [model] = step(initialModel()[0], { kind: 'settings_open' });
  [model] = step(model, { kind: 'appearance_failed', error: bytes('engine unavailable') });
  assert.equal(model.settingsOpen, true);
  assert.match(text(model.appearance.notice), /unavailable/);
  let cmd;
  [model, cmd] = step(model, { kind: 'settings_close' });
  assert.equal(model.settingsOpen, false);
  assert.equal(cmd.name, 'cockpit.committed');
  // A failed Save is not a dismissal: the active preview stays cancellable.
  [model] = step(opened(), { kind: 'settings_commit' });
  [model] = step(model, { kind: 'appearance_failed', error: bytes('write failed') });
  assert.equal(model.settingsOpen, true);
  assert.equal(model.appearanceBusy, false);
});

test('the placement menu command previews through the appearance transaction', () => {
  const model = opened();
  const [previewing, cmd] = step(model, { kind: 'toggle_tab_placement' });
  assert.equal(cmd.name, 'cockpit.appearance');
  assert.deepEqual([...cmd.payload], [1, 5, 1]);
  assert.equal(previewing.tabPlacement, model.tabPlacement);
  assert.deepEqual(step(previewing, { kind: 'toggle_tab_placement' }), [previewing, null]);
  const [rail] = step(previewing, { kind: 'appearance_loaded', body: reply({ placement: 1, dirty: true }) });
  assert.deepEqual([...step(rail, { kind: 'toggle_tab_placement' })[1].payload], [1, 5, 0]);
  // Outside Settings the command keeps its direct intent.
  const [, direct] = step(initialModel()[0], { kind: 'toggle_tab_placement' });
  assert.equal(direct.name, 'cockpit.intent');
});

test('Workspace and Connection arrow keys never change a hidden theme', () => {
  for (const section of [1, 2]) {
    const model = step(opened(), { kind: 'settings_section', section })[0];
    assert.deepEqual(step(model, { kind: 'settings_move', delta: 1 }), [model, null]);
  }
});
