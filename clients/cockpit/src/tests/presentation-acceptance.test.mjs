import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update, windows } from '../core.ts';
import { declaredDensities, declaredSizes, inspectShippingGallery, passiveHoverRendererTest,
  assertPointerReachedSurface } from '../../scripts/cockpit-state-gallery.mjs';

const strict = process.env.PHUX_COCKPIT_ACCEPTANCE_STRICT === '1';
const bytes = value => new TextEncoder().encode(value);
const text = value => new TextDecoder().decode(value);
const revision = { hi: 0, lo: 19 };
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
const knownRed = (name, fn) => test(name,
  strict ? {} : { todo: 'known red on the Wave-1 base; set PHUX_COCKPIT_ACCEPTANCE_STRICT=1 after lifecycle integration' }, fn);

const surfaces = [
  { name: 'terminals', msg: { kind: 'palette_open' }, matches: model => model.paletteOpen && !model.agentsMode && model.navigatorView === 0 },
  { name: 'agents', msg: { kind: 'agents_open' }, matches: model => model.paletteOpen && model.agentsMode },
  { name: 'sessions', msg: { kind: 'sessions_open' }, matches: model => model.paletteOpen && !model.agentsMode && model.navigatorView === 1 },
  { name: 'machines', msg: { kind: 'machines_open' }, matches: model => model.paletteOpen && !model.agentsMode && model.navigatorView === 2 },
  { name: 'windows', msg: { kind: 'windows_open' }, matches: model => model.paletteOpen && !model.agentsMode && model.navigatorView === 3 },
  { name: 'commands', msg: { kind: 'commands_open' }, matches: model => model.paletteOpen && !model.agentsMode && model.navigatorView === 4 },
  { name: 'new-session', msg: { kind: 'new_session_open' }, matches: model => model.renameOpen && model.creatingSession },
  { name: 'add-machine', msg: { kind: 'add_machine_open' }, matches: model => model.hostOpen && model.toolPurpose === 1 },
  { name: 'directory', msg: { kind: 'dir_open' }, matches: model => model.dirOpen },
  { name: 'rename', msg: { kind: 'rename_open' }, matches: model => model.renameOpen && !model.creatingSession },
  { name: 'connection', msg: { kind: 'host_open' }, matches: model => model.hostOpen && model.toolPurpose === 0 },
  { name: 'settings', msg: { kind: 'settings_open' }, matches: model => model.settingsOpen },
];

const ownerFields = prefix => [
  `${prefix}PaletteOpen`, `${prefix}AgentsOpen`, `${prefix}SettingsOpen`, `${prefix}HostOpen`,
  `${prefix}DirOpen`, `${prefix}RenameOpen`, `${prefix}EmptyOpen`,
];
const everyOwnerField = ['main', 'window1', 'window2', 'window3', 'window4'].flatMap(ownerFields);

function base(window = 0) {
  const open = {};
  if (window > 0) open[`window${window}Open`] = true;
  return { ...initialModel()[0], ...open, activeWindow: window, engineConnected: true, engineRevision: revision };
}

function openSurface(model, surface) {
  let [next] = step(model, surface.msg);
  // Opening Settings/New Session starts a real async transaction. A failed
  // opening reply leaves the surface available for transition testing without
  // inventing a successful engine payload.
  if (surface.name === 'settings' && next.appearanceBusy) [next] = step(next, { kind: 'appearance_failed', error: bytes('fixture unavailable') });
  if (surface.name === 'new-session' && next.renameBusy) [next] = step(next, { kind: 'new_session_failed', error: bytes('fixture unavailable') });
  return next;
}

function settleDeparture(model) {
  let next = model;
  for (let attempts = 0; attempts < 3; attempts += 1) {
    if (next.pendingSettingsAction !== null) [next] = step(next, { kind: 'appearance_failed', error: bytes('fixture rollback') });
    else if (next.pendingSessionAction !== null) [next] = step(next, { kind: 'new_session_failed', error: bytes('fixture cancellation') });
    else break;
  }
  return next;
}

function visibleSurfaces(model) {
  return surfaces.filter(surface => surface.matches(model)).map(surface => surface.name);
}

knownRed('every surface-to-surface transition replaces the previous surface', () => {
  const failures = [];
  for (const source of surfaces) {
    const before = openSurface(base(), source);
    assert.equal(source.matches(before), true, `fixture could not open ${source.name}`);
    for (const destination of surfaces) {
      const after = settleDeparture(step(before, destination.msg)[0]);
      const visible = visibleSurfaces(after);
      if (!destination.matches(after) || visible.length !== 1) {
        failures.push(`${source.name} -> ${destination.name}: visible=[${visible.join(', ')}], ` +
          `palette=${after.paletteOpen}, agentsMode=${after.agentsMode}, navigatorView=${after.navigatorView}, ` +
          `settings=${after.settingsOpen}, host=${after.hostOpen}, dir=${after.dirOpen}, ` +
          `rename=${after.renameOpen}, creatingSession=${after.creatingSession}`);
      }
    }
  }
  assert.deepEqual(failures, [], `surface replacement failures:\n${failures.map(item => `  - ${item}`).join('\n')}`);
});

test('each surface is projected only in its captured owner window', () => {
  for (let owner = 0; owner <= 4; owner += 1) {
    for (const surface of surfaces) {
      const model = openSurface(base(owner), surface);
      assert.equal(surface.matches(model), true, `${surface.name} did not open for owner ${owner}`);
      const openFields = everyOwnerField.filter(field => model[field]);
      const prefix = owner === 0 ? 'main' : `window${owner}`;
      assert.ok(openFields.length >= 1, `${surface.name} has no projected owner at window ${owner}`);
      assert.equal(openFields.every(field => field.startsWith(prefix)), true,
        `${surface.name} owner ${owner} leaked through ${openFields.join(', ')}`);
    }
  }
});

knownRed('OS close withdraws the owner window immediately across pending operations', () => {
  const failures = [];
  for (const surface of surfaces) {
    let model = openSurface(base(2), surface);
    // Exercise a pending operation in every surface without fabricating an
    // outcome. Surface-specific failure replies below are deliberately stale
    // after close and must not resurrect the presentation.
    if (surface.name === 'settings') model = { ...model, appearanceBusy: false };
    const [closed] = step(model, { kind: 'window_closed', window: 2 });
    const declared = windows(closed).map(window => text(window.label));
    const leaked = ownerFields('window2').filter(field => closed[field]);
    if (closed.window2Open || declared.includes('phux-window-2') || leaked.length > 0) {
      failures.push(`${surface.name}: open=${closed.window2Open}, descriptors=[${declared.join(', ')}], ownerFlags=[${leaked.join(', ')}]`);
    }
    const staleReplies = [
      { kind: 'navigation_failed', error: bytes('late') }, { kind: 'machines_failed', error: bytes('late') },
      { kind: 'keybindings_failed', error: bytes('late') }, { kind: 'appearance_failed', error: bytes('late') },
      { kind: 'remote_failed', error: bytes('late') }, { kind: 'directory_failed', error: bytes('late') },
      { kind: 'session_failed', error: bytes('late') }, { kind: 'new_session_failed', error: bytes('late') },
      { kind: 'window_action_failed', error: bytes('late') },
    ];
    let after = closed;
    for (const reply of staleReplies) [after] = step(after, reply);
    if (after.window2Open || windows(after).some(window => text(window.label) === 'phux-window-2')) {
      failures.push(`${surface.name}: a late reply resurrected phux-window-2`);
    }
  }
  assert.deepEqual(failures, [], `window-close lifecycle failures:\n${failures.map(item => `  - ${item}`).join('\n')}`);
});

test('windows enumerates all slot combinations and invalid closes mutate none', () => {
  for (let mask = 0; mask < 16; mask += 1) {
    const model = { ...base(), window1Open: Boolean(mask & 1), window2Open: Boolean(mask & 2),
      window3Open: Boolean(mask & 4), window4Open: Boolean(mask & 8) };
    assert.deepEqual(windows(model).map(window => text(window.label)), [1, 2, 3, 4]
      .filter(index => mask & (1 << (index - 1))).map(index => `phux-window-${index}`), `mask ${mask.toString(2).padStart(4, '0')}`);
    for (const invalid of [-1, 0, 1.5, 5, 255]) {
      const [after] = step(model, { kind: 'window_closed', window: invalid });
      assert.deepEqual(windows(after).map(window => text(window.label)), windows(model).map(window => text(window.label)),
        `invalid close ${invalid} changed mask ${mask}`);
    }
  }
});

knownRed('an invalid or closed active-window combination never projects a modal into that slot', () => {
  const failures = [];
  for (const activeWindow of [-1, 1, 4, 5, 1.5]) {
    for (const surface of surfaces) {
      const model = openSurface({ ...base(), activeWindow }, surface);
      const leaked = activeWindow >= 1 && activeWindow <= 4
        ? ownerFields(`window${Math.trunc(activeWindow)}`).filter(field => model[field]) : [];
      if (leaked.length > 0) failures.push(`activeWindow=${activeWindow}, ${surface.name}: ${leaked.join(', ')}`);
    }
  }
  assert.deepEqual(failures, [], `closed-slot projection failures:\n${failures.map(item => `  - ${item}`).join('\n')}`);
});

test('state gallery covers shipping controls, declared geometry, and the passive-hover acceptance split', () => {
  const root = new URL('../..', import.meta.url);
  const report = inspectShippingGallery(root);
  assert.deepEqual(report.missing, []);
  assert.deepEqual(report.states, ['rest', 'hover', 'press', 'selected', 'focus', 'disabled', 'attention', 'loading', 'empty', 'failed', 'passive-hover']);
  assert.deepEqual(declaredSizes, [{ width: 900, height: 420 }, { width: 1100, height: 640 }, { width: 1680, height: 1000 }]);
  assert.deepEqual(declaredDensities, ['compact', 'regular', 'spacious']);
  assert.equal(passiveHoverRendererTest,
    'semantic_theme test "passive panel hover is visually stable without disabling hit testing"');
  assert.equal(report.finalZigGate, `full Zig gate must include ${passiveHoverRendererTest}`);
  assert.match(report.evidenceScope.passiveHover, /pointer reachability only/);
  assert.match(report.evidenceScope.liveScreenshot, /optional only.*never use full-frame PNG equality/);
  const audit = readFileSync(new URL('../native_extension.zig', import.meta.url), 'utf8');
  for (const { width, height } of declaredSizes) assert.match(audit, new RegExp(`SizeF\\.init\\(${width}, ${height}\\)`));
  for (const density of declaredDensities) assert.match(audit, new RegExp(`\\.${density}`));
  assert.doesNotThrow(() => assertPointerReachedSurface(
    'widget @w1/canvas#7 role=group name="Inspector surface" bounds=(0,0 100x80) focused=false enabled=true state=[hovered]',
    { name: 'Inspector surface' }));
});
