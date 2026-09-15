import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update, windows } from '../core.ts';
import { declaredDensities, declaredSizes, inspectShippingStateInventory,
  strictAcceptanceCommand, assertPointerReachedSurface } from '../../scripts/cockpit-state-inventory.mjs';

// Every case is mandatory. The historical strict entry point remains a stable
// documented command and must pass the same matrix:
// PHUX_COCKPIT_ACCEPTANCE_STRICT=1 node --import ./src/tests/navigation-loader.mjs --test src/tests/presentation-acceptance.test.mjs
const bytes = value => new TextEncoder().encode(value);
const text = value => new TextDecoder().decode(value);
const revision = { hi: 0, lo: 19 };
const token = new Uint8Array([42, 0, 0, 0, 0, 0, 0, 0]);

const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
const effects = cmd => !cmd ? [] : cmd.op === 'batch' ? cmd.cmds.flatMap(effects) : [cmd];
const request = (cmd, name) => effects(cmd).find(effect => effect.op === 'request' && effect.name === name);
const sameBytes = (left, right) => left.length === right.length && left.every((byte, index) => byte === right[index]);

const surfaces = [
  { name: 'terminals', msg: { kind: 'palette_open' }, request: 'cockpit.navigation', matches: model => model.paletteOpen && !model.agentsMode && model.navigatorView === 0 },
  { name: 'agents', msg: { kind: 'agents_open' }, request: 'cockpit.navigation', matches: model => model.paletteOpen && model.agentsMode },
  { name: 'sessions', msg: { kind: 'sessions_open' }, request: 'cockpit.navigation', matches: model => model.paletteOpen && !model.agentsMode && model.navigatorView === 1 },
  { name: 'machines', msg: { kind: 'machines_open' }, request: 'cockpit.machines', matches: model => model.paletteOpen && !model.agentsMode && model.navigatorView === 2 },
  { name: 'windows', msg: { kind: 'windows_open' }, request: 'cockpit.navigation', matches: model => model.paletteOpen && !model.agentsMode && model.navigatorView === 3 },
  { name: 'commands', msg: { kind: 'commands_open' }, request: 'cockpit.keybindings', matches: model => model.paletteOpen && !model.agentsMode && model.navigatorView === 4 },
  { name: 'new-session', msg: { kind: 'new_session_open' }, request: 'cockpit.new-session', matches: model => model.renameOpen && model.creatingSession },
  { name: 'add-machine', msg: { kind: 'add_machine_open' }, request: 'cockpit.local-tools', matches: model => model.hostOpen && model.toolPurpose === 1 },
  { name: 'directory', msg: { kind: 'dir_open' }, request: 'cockpit.directory', matches: model => model.dirOpen },
  { name: 'rename', msg: { kind: 'rename_open' }, request: 'cockpit.session', matches: model => model.renameOpen && !model.creatingSession },
  { name: 'connection', msg: { kind: 'host_open' }, request: 'cockpit.remote', matches: model => model.hostOpen && model.toolPurpose === 0 },
  { name: 'settings', msg: { kind: 'settings_open' }, request: 'cockpit.appearance', matches: model => model.settingsOpen },
];

const ownerFields = prefix => [
  `${prefix}PaletteOpen`, `${prefix}AgentsOpen`, `${prefix}SettingsOpen`, `${prefix}HostOpen`,
  `${prefix}DirOpen`, `${prefix}RenameOpen`, `${prefix}EmptyOpen`,
];
const everyOwnerField = ['main', 'window1', 'window2', 'window3', 'window4'].flatMap(ownerFields);

function ownerField(surface, prefix = 'main') {
  if (surface.name === 'agents') return `${prefix}AgentsOpen`;
  if (['terminals', 'sessions', 'machines', 'windows', 'commands'].includes(surface.name)) return `${prefix}PaletteOpen`;
  if (surface.name === 'settings') return `${prefix}SettingsOpen`;
  if (['add-machine', 'connection'].includes(surface.name)) return `${prefix}HostOpen`;
  if (surface.name === 'directory') return `${prefix}DirOpen`;
  return `${prefix}RenameOpen`;
}

function record(kind, body) {
  return [kind, body.length % 256, Math.floor(body.length / 256), ...body];
}

function contextsRecord(windows) {
  const entries = windows.flatMap(window => [window, 0, 2, 0, 0]);
  return record(5, [1, windows.length, ...entries]);
}

function snapshotBytes(secondary = []) {
  const head = new Uint8Array(28);
  head[0] = 1;
  head[1] = 2;
  head[10] = revision.lo;
  head[23] = 2;
  head[26] = 168;
  const sections = secondary.flatMap(index => [index, 0, 0, 0, 0, 168, 0]);
  const contexts = contextsRecord([0, ...secondary]);
  return new Uint8Array([...head, 0, 255, 0, 0, secondary.length, ...sections, 0, 0, 0, 0, 0, ...contexts]);
}

function base(owner = 0) {
  const [loaded] = step(initialModel()[0], { kind: 'snapshot_loaded', body: snapshotBytes(owner === 0 ? [] : [owner]) });
  return { ...loaded, activeWindow: owner, engineConnected: true, engineRevision: revision };
}

function appearanceReply(active, dirty = false, outcome = 0) {
  return new Uint8Array([1, +active, outcome, +dirty, 0, 0, 0, 0, 0, 0]);
}

function sessionReply(phase) {
  return new Uint8Array([1, phase, ...token, 1, 0, 0, 0, 4, ...bytes('mini'), 0]);
}

function requireRequest(cmd, name, label) {
  const emitted = request(cmd, name);
  assert.ok(emitted, `${label} did not emit ${name}`);
  return emitted;
}

function openSurface(model, surface) {
  let cmd;
  [model, cmd] = step(model, surface.msg);
  const emitted = requireRequest(cmd, surface.request, `opening ${surface.name}`);
  if (surface.name === 'new-session') {
    assert.equal(emitted.payload[1], 1, 'New Session must begin with Describe');
    [model] = step(model, { kind: emitted.okKind, body: sessionReply(0) });
  } else if (surface.name === 'settings') {
    assert.deepEqual([...emitted.payload], [1, 0, 0], 'Settings must begin a native appearance transaction');
    [model] = step(model, { kind: emitted.okKind, body: appearanceReply(true, true) });
  }
  assert.equal(surface.matches(model), true, `fixture could not open ${surface.name}`);
  return model;
}

function visibleSurfaces(model) {
  return surfaces.filter(surface => surface.matches(model)).map(surface => surface.name);
}

function presentationSignature(model) {
  const openOwners = everyOwnerField.filter(field => model[field]);
  return `visible=[${visibleSurfaces(model).join(',')}];palette=${+model.paletteOpen};agents=${+model.agentsMode};` +
    `view=${model.navigatorView};settings=${+model.settingsOpen};host=${+model.hostOpen};purpose=${model.toolPurpose};` +
    `dir=${+model.dirOpen};rename=${+model.renameOpen};creating=${+model.creatingSession};ownerFields=[${openOwners.join(',')}]`;
}

function completeSourceDeparture(source, destination, model, cmd) {
  if (source.name === 'new-session') {
    const cancel = request(cmd, 'cockpit.new-session');
    if (!cancel) return { model, failure: `cancel=missing;${presentationSignature(model)}` };
    assert.equal(cancel.okKind, 'new_session_cancelled');
    if (cancel.payload[1] !== 4 || !sameBytes(cancel.payload.slice(2, 10), token)) {
      return { model, failure: `cancel=malformed;${presentationSignature(model)}` };
    }
    [model] = step(model, { kind: cancel.okKind, body: sessionReply(0) });
  } else if (source.name === 'settings' && destination.name !== 'settings') {
    const rollback = request(cmd, 'cockpit.appearance');
    if (!rollback) return { model, failure: `rollback=missing;${presentationSignature(model)}` };
    if (rollback.payload[1] !== 6) return { model, failure: `rollback=malformed;${presentationSignature(model)}` };
    [model] = step(model, { kind: rollback.okKind, body: appearanceReply(false, false, 2) });
  }
  return { model, failure: null };
}

function transitionFailure(source, destination) {
  const before = openSurface(base(), source);
  let [after, cmd] = step(before, destination.msg);
  const departure = completeSourceDeparture(source, destination, after, cmd);
  after = departure.model;
  if (departure.failure) return departure.failure;
  const visible = visibleSurfaces(after);
  const openOwners = everyOwnerField.filter(field => after[field]);
  const destinationOwner = ownerField(destination);
  const exactDestinationOwner = openOwners.length === 1 && openOwners[0] === destinationOwner;
  const sourceOwner = ownerField(source);
  const staleSourceOwner = sourceOwner !== destinationOwner && openOwners.includes(sourceOwner);
  return destination.matches(after) && visible.length === 1 && exactDestinationOwner && !staleSourceOwner
    ? null : presentationSignature(after);
}

function descriptorPresent(model, owner) {
  const label = `phux-window-${owner}`;
  return model[`window${owner}Open`] || windows(model).some(window => text(window.label) === label);
}

function withdrawalFailure(model, owner) {
  const leaked = ownerFields(`window${owner}`).filter(field => model[field]);
  return descriptorPresent(model, owner) || leaked.length > 0
    ? `descriptor=${+descriptorPresent(model, owner)};ownerFlags=[${leaked.join(',')}]`
    : null;
}

function openPendingSurface(surface, owner = 2) {
  let model = base(owner);
  let cmd;
  [model, cmd] = step(model, surface.msg);
  const emitted = requireRequest(cmd, surface.request, `opening pending ${surface.name}`);
  if (surface.name === 'new-session') {
    assert.equal(emitted.payload[1], 1);
    [model] = step(model, { kind: emitted.okKind, body: sessionReply(0) });
    return { model, pending: null };
  }
  if (surface.name === 'settings') {
    assert.deepEqual([...emitted.payload], [1, 0, 0]);
    [model] = step(model, { kind: emitted.okKind, body: appearanceReply(true, true) });
    return { model, pending: null };
  }
  return { model, pending: emitted };
}

function recordWithdrawal(failures, stage, model, owner) {
  const failure = withdrawalFailure(model, owner);
  if (failure) failures.push(`${stage}:${failure}`);
}

function windowCloseFailure(surface) {
  const owner = 2;
  const opened = openPendingSurface(surface, owner);
  let model;
  let cmd;
  [model, cmd] = step(opened.model, { kind: 'window_closed', window: owner });
  const failures = [];
  recordWithdrawal(failures, 'immediate', model, owner);

  if (surface.name === 'settings') {
    const rollback = requireRequest(cmd, 'cockpit.appearance', 'Settings window close');
    assert.equal(rollback.payload[1], 6);
    [model, cmd] = step(model, { kind: rollback.okKind, body: appearanceReply(false, false, 2) });
    recordWithdrawal(failures, 'after-rollback', model, owner);
  } else if (surface.name === 'new-session') {
    const cancel = requireRequest(cmd, 'cockpit.new-session', 'New Session window close');
    assert.equal(cancel.okKind, 'new_session_cancelled');
    assert.equal(cancel.payload[1], 4);
    assert.deepEqual(cancel.payload.slice(2, 10), token);
    [model, cmd] = step(model, { kind: cancel.okKind, body: sessionReply(0) });
    recordWithdrawal(failures, 'after-cancel', model, owner);
  }

  const refresh = requireRequest(cmd, 'cockpit.snapshot', `${surface.name} window close refresh`);
  [model, cmd] = step(model, { kind: refresh.okKind, body: snapshotBytes() });
  recordWithdrawal(failures, 'after-refresh', model, owner);
  assert.equal(request(cmd, 'cockpit.snapshot'), undefined, 'one correlated refresh must settle the close');

  if (opened.pending) {
    [model] = step(model, { kind: opened.pending.errKind, error: bytes('late opening reply') });
    recordWithdrawal(failures, 'after-late-opening-reply', model, owner);
  }
  return failures.length === 0 ? null : failures.join('|');
}

function failedOpenDepartureFailure() {
  let model;
  let cmd;
  [model, cmd] = step(base(), { kind: 'new_session_open' });
  const describe = requireRequest(cmd, 'cockpit.new-session', 'failed New Session open');
  assert.equal(describe.payload[1], 1);
  [model] = step(model, { kind: describe.errKind, error: bytes('describe failed') });
  [model, cmd] = step(model, { kind: 'sessions_open' });
  const unexpected = request(cmd, 'cockpit.new-session');
  const complete = model.pendingSessionAction === null && !model.renameOpen && surfaces[2].matches(model);
  if (complete && !unexpected) return null;
  return `cancelRequest=${unexpected ? unexpected.payload[1] : 'none'};pending=${model.pendingSessionAction ? model.pendingSessionAction.code : 'none'};${presentationSignature(model)}`;
}

function assertAcceptanceCase(id, failure) {
  assert.equal(baselineExemptions.has(id), false, `${id} must not be exempted`);
  assert.equal(failure, null, `${id}: ${failure}`);
}

const baselineExemptions = new Set();

test('acceptance matrix has no baseline exemptions', () => {
  assert.equal(baselineExemptions.size, 0);
});

for (const source of surfaces) {
  for (const destination of surfaces) {
    const id = `transition:${source.name}->${destination.name}`;
    test(id, () => assertAcceptanceCase(id, transitionFailure(source, destination)));
  }
}

for (let owner = 0; owner <= 4; owner += 1) {
  for (const surface of surfaces) {
    const id = `owner:${owner}:${surface.name}`;
    test(id, () => {
      const model = openSurface(base(owner), surface);
      const open = everyOwnerField.filter(field => model[field]);
      const prefix = owner === 0 ? 'main' : `window${owner}`;
      const failure = open.length > 0 && open.every(field => field.startsWith(prefix)) ? null : `ownerFlags=[${open.join(',')}]`;
      assertAcceptanceCase(id, failure);
    });
  }
}

for (const activeWindow of [1, 2, 3, 4]) {
  for (const surface of surfaces) {
    const id = `closed-owner:${activeWindow}:${surface.name}`;
    test(id, () => {
      const model = openSurface({ ...base(), activeWindow }, surface);
      const leaked = activeWindow >= 1 && activeWindow <= 4
        ? ownerFields(`window${Math.trunc(activeWindow)}`).filter(field => model[field]) : [];
      assertAcceptanceCase(id, leaked.length === 0 ? null : `ownerFlags=[${leaked.join(',')}]`);
    });
  }
}

for (const surface of surfaces) {
  const id = `window-close:${surface.name}`;
  test(id, () => assertAcceptanceCase(id, windowCloseFailure(surface)));
}

test('new-session:failed-open->sessions', () => {
  const id = 'new-session:failed-open->sessions';
  assertAcceptanceCase(id, failedOpenDepartureFailure());
});

for (let mask = 0; mask < 16; mask += 1) {
  test(`windows:mask:${mask.toString(2).padStart(4, '0')}`, () => {
    const model = { ...base(), window1Open: Boolean(mask & 1), window2Open: Boolean(mask & 2),
      window3Open: Boolean(mask & 4), window4Open: Boolean(mask & 8) };
    assert.deepEqual(windows(model).map(window => text(window.label)), [1, 2, 3, 4]
      .filter(index => mask & (1 << (index - 1))).map(index => `phux-window-${index}`));
  });
}

for (const invalid of [-1, 0, 5, 255]) {
  test(`windows:invalid-close:${invalid}`, () => {
    const model = { ...base(), window1Open: true, window3Open: true };
    const [after] = step(model, { kind: 'window_closed', window: invalid });
    assert.deepEqual(windows(after).map(window => text(window.label)), ['phux-window-1', 'phux-window-3']);
  });
}

test('shipping state inventory names declarations and reserves rendered fixtures for native integration', () => {
  const root = new URL('../..', import.meta.url);
  const report = inspectShippingStateInventory(root);
  assert.equal(report.kind, 'shipping-state-inventory');
  assert.deepEqual(report.missing, []);
  assert.deepEqual(report.states, ['rest', 'hover', 'press', 'selected', 'focus', 'disabled', 'attention', 'loading', 'empty', 'failed', 'passive-hover']);
  assert.deepEqual(declaredSizes, [{ width: 900, height: 420 }, { width: 1100, height: 640 }, { width: 1680, height: 1000 }]);
  assert.deepEqual(declaredDensities, ['compact', 'regular', 'spacious']);
  assert.equal(strictAcceptanceCommand,
    'PHUX_COCKPIT_ACCEPTANCE_STRICT=1 node --import ./src/tests/navigation-loader.mjs --test src/tests/presentation-acceptance.test.mjs');
  assert.equal(report.strictAcceptanceCommand, strictAcceptanceCommand);
  assert.deepEqual(report.unrenderedStates, report.states);
  assert.match(report.crossCommitZigRequirement,
    /^full Zig gate must include semantic_theme test "passive panel hover is visually stable without disabling hit testing"$/);
  assert.equal(report.renderedStateGallery, 'reserved for compiled native integration Wave 2');
  assert.match(report.evidenceScope.passiveHover, /pointer reachability only/);
  assert.match(report.evidenceScope.liveScreenshot, /optional only.*never use full-frame PNG equality/);
  const audit = readFileSync(new URL('../native_extension.zig', import.meta.url), 'utf8');
  for (const { width, height } of declaredSizes) assert.match(audit, new RegExp(`SizeF\\.init\\(${width}, ${height}\\)`));
  for (const density of declaredDensities) assert.match(audit, new RegExp(`\\.${density}`));
  assert.doesNotThrow(() => assertPointerReachedSurface(
    'widget @w1/canvas#7 role=group name="Inspector surface" bounds=(0,0 100x80) focused=false enabled=true state=[hovered]',
    { name: 'Inspector surface' }));
});
