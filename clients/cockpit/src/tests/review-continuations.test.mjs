import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';
import { navigationScopedRequest, navigationPage } from '../protocol.ts';
const bytes = text => new TextEncoder().encode(text);
const step = (model, msg) => { const value = update(model, msg); return Array.isArray(value) ? value : [value, null]; };
const effects = cmd => !cmd ? [] : cmd.op === 'batch' ? cmd.cmds.flatMap(effects) : [cmd];
const committed = cmd => effects(cmd).some(effect => effect.name === 'cockpit.committed');
const request = (cmd, name) => effects(cmd).find(effect => effect.name === name);
const appearance = (active = true, dirty = false) => new Uint8Array([1, +active, 0, +dirty, 255, 0, 0, 0, 0, 0]);
const settings = () => step(step(initialModel()[0], { kind: 'settings_open' })[0], { kind: 'appearance_loaded', body: appearance(true, true) })[0];
const token = new Uint8Array([42, 0, 0, 0, 0, 0, 0, 0]);
const session = phase => new Uint8Array([1, phase, ...token, 1, 0, 0, 0, 4, ...bytes('mini'), 0]);
function pendingSession() {
  let [model] = step(initialModel()[0], { kind: 'new_session_open' });
  [model] = step(model, { kind: 'new_session_loaded', body: session(0) });
  return step(model, { kind: 'new_session_loaded', body: session(1) })[0];
}
const u32 = n => [n & 255, n >>> 8 & 255, n >>> 16 & 255, n >>> 24 & 255];
const field = text => [text.length, 0, ...bytes(text)];
function inventory(id, connection = 2) {
  return new Uint8Array([1, 0, ...u32(id), ...u32(3), ...u32(1), ...u32(0), 1, 0, ...field(''),
    ...u32(0), 0, 0, connection, ...field('This Mac'), ...field(''), ...field(''), ...field('')]);
}

function snapshotMessage(revision = 7) {
  const body = new Uint8Array(33);
  body[0] = 1; body[1] = 2; body[10] = revision; body[23] = 2; body[26] = 168; body[29] = 255;
  return { kind: 'snapshot_loaded', body };
}

function machineInventory(id, first = 0, count = 1, total = 1, connection = 0) {
  const rows = Array.from({ length: count }, (_, at) => {
    const index = first + at;
    return [...u32(index), 1, 1, connection, ...field(`machine-${index}`), ...field(`host-${index}`), ...field(''), ...field('')];
  }).flat();
  return new Uint8Array([1, 0, ...u32(id), ...u32(3), ...u32(total), ...u32(first), count, 0, ...field(''), ...rows]);
}

function listedMachines(count = 1) {
  let [model] = step(initialModel()[0], { kind: 'machines_open' });
  [model] = step(model, { kind: 'machines_loaded', body: machineInventory(model.machines.requestId, 0, Math.min(64, count), count) });
  if (count > 64) {
    [model] = step(model, { kind: 'machines_more' });
    [model] = step(model, { kind: 'machines_loaded', body: machineInventory(model.machines.requestId, 64, count - 64, count) });
  }
  return model;
}

test('Commands Toggle Tab Placement commits its departure before native intent', () => {
  let [model] = step(initialModel()[0], { kind: 'commands_open' });
  [model] = step(model, { kind: 'palette_edit', edit: { kind: 'insert_text', text: bytes('Toggle Tab Placement') } });
  const [closed, cmd] = step(model, { kind: 'palette_submit' });
  assert.equal(closed.paletteOpen, false);
  assert.ok(committed(cmd));
});

test('Commands queue refusal still commits its closed input gate', () => {
  let model = initialModel()[0];
  model = { ...model, tabCommands: { ...model.tabCommands, queue: Array.from({ length: 16 }, () => ({ id: { hi: 0, lo: 1 }, bytes: new Uint8Array(0) })) } };
  [model] = step(model, { kind: 'commands_open' });
  [model] = step(model, { kind: 'palette_edit', edit: { kind: 'insert_text', text: bytes('New Tab') } });
  const [closed, cmd] = step(model, { kind: 'palette_submit' });
  assert.equal(closed.paletteOpen, false);
  assert.ok(committed(cmd));
});

test('replacing New Session cancels captured native focus authority first', () => {
  for (const kind of ['sessions_open', 'commands_open', 'machines_open', 'settings_open', 'new_session_open', 'new_window']) {
    const [model, cmd] = step(pendingSession(), { kind });
    const cancel = request(cmd, 'cockpit.new-session');
    assert.equal(cancel?.payload[1], 4, kind);
    assert.deepEqual(cancel.payload.slice(2, 10), token);
    assert.equal(model.newSessionAwaiting, false);
  }
});

test('deferred Close Pane, New Window and New Tab refuse an intervening snapshot revision', () => {
  for (const action of [{ kind: 'native_command', command: 3 }, { kind: 'new_window' }, { kind: 'new_terminal' }]) {
    let [model] = step(pendingSession(), action);
    assert.equal(model.pendingSessionAction.revision.lo, model.engineRevision.lo);
    const capturedRevision = model.pendingSessionAction.revision.lo;
    [model] = step(model, snapshotMessage());
    assert.equal(model.engineRevision.lo, 7);
    assert.equal(model.pendingSessionAction.revision.lo, capturedRevision, 'snapshots never refresh captured authority');
    const [refused, cmd] = step(model, { kind: 'new_session_cancelled', body: session(0) });
    assert.equal(request(cmd, 'cockpit.tab-command'), undefined, action.kind);
    assert.equal(request(cmd, 'cockpit.intent'), undefined, action.kind);
    assert.match(new TextDecoder().decode(refused.commandNotice), /context changed/i);
  }
});

test('unchanged deferred command context still admits the originally requested action', () => {
  for (const action of [{ kind: 'native_command', command: 3 }, { kind: 'new_window' }, { kind: 'new_terminal' }]) {
    const [waiting] = step(pendingSession(), action);
    const [resumed, cmd] = step(waiting, { kind: 'new_session_cancelled', body: session(0) });
    assert.equal(resumed.pendingSessionAction, null);
    assert.ok(request(cmd, 'cockpit.tab-command'), action.kind);
  }
});

test('a refused deferred Settings command still commits the completed modal departure', () => {
  let [model] = step(settings(), { kind: 'new_window' });
  [model] = step(model, snapshotMessage());
  const [refused, cmd] = step(model, { kind: 'appearance_loaded', body: appearance(false) });
  assert.equal(refused.settingsOpen, false);
  assert.ok(committed(cmd));
  assert.equal(request(cmd, 'cockpit.tab-command'), undefined);
  assert.match(new TextDecoder().decode(refused.commandNotice), /context changed/i);
});

test('Forget confirmation survives read-only status and remains usable during a poll', () => {
  let model = listedMachines();
  const target = model.machineRows[0].target.slice();
  [model] = step(model, { kind: 'machine_forget', target });
  [model] = step(model, snapshotMessage());
  const polling = model;
  [model] = step(model, { kind: 'machines_loaded', body: machineInventory(model.machines.requestId) });
  assert.deepEqual(model.machines.forgetTarget, target);
  const [, cmd] = step(polling, { kind: 'machine_forget_confirm' });
  assert.equal(request(cmd, 'cockpit.machines')?.payload[1], 5);
  assert.deepEqual(request(cmd, 'cockpit.machines').payload.slice(6, 14), target);
});

test('status changes retire Forget when the captured identity or capability changes', () => {
  for (const change of ['connected', 'identity']) {
    let model = listedMachines();
    [model] = step(model, { kind: 'machine_forget', target: model.machineRows[0].target });
    [model] = step(model, snapshotMessage());
    const body = machineInventory(model.machines.requestId, 0, 1, 1, change === 'connected' ? 2 : 0);
    if (change === 'identity') body[31] = 'X'.charCodeAt(0);
    [model] = step(model, { kind: 'machines_loaded', body });
    assert.equal(model.machines.forgetTarget.length, 0);
    const [, cmd] = step(model, { kind: 'machine_forget_confirm' });
    assert.equal(request(cmd, 'cockpit.machines'), undefined);
  }
});

test('a non-progressing status page fails closed instead of starting an infinite sweep', () => {
  let model = listedMachines(65);
  [model] = step(model, snapshotMessage());
  const [failed, cmd] = step(model, { kind: 'machines_loaded', body: machineInventory(model.machines.requestId, 0, 0, 65) });
  assert.equal(failed.machines.failed, true);
  assert.equal(failed.machines.loading, false);
  assert.equal(request(cmd, 'cockpit.machines'), undefined);
});

test('a status sweep drains 65 loaded rows and replays an in-flight invalidation without more snapshots', () => {
  let model = { ...listedMachines(65), navigatorScroll: 400 };
  const selected = model.machineRows[64].target.slice();
  model = { ...model, machines: { ...model.machines, selected } };
  [model] = step(model, snapshotMessage());
  [model] = step(model, snapshotMessage(8));
  let cmd;
  [model, cmd] = step(model, { kind: 'machines_loaded', body: machineInventory(model.machines.requestId, 0, 64, 65, 1) });
  assert.equal(request(cmd, 'cockpit.machines')?.payload[10], 64);
  [model, cmd] = step(model, { kind: 'machines_loaded', body: machineInventory(model.machines.requestId, 64, 1, 65, 2) });
  assert.equal(request(cmd, 'cockpit.machines')?.payload[10], 0, 'the retained invalidation starts one fresh sweep');
  [model, cmd] = step(model, { kind: 'machines_loaded', body: machineInventory(model.machines.requestId, 0, 64, 65, 2) });
  assert.equal(request(cmd, 'cockpit.machines')?.payload[10], 64);
  [model, cmd] = step(model, { kind: 'machines_loaded', body: machineInventory(model.machines.requestId, 64, 1, 65, 2) });
  assert.equal(request(cmd, 'cockpit.machines'), undefined);
  assert.equal(model.machines.loading, false);
  assert.equal(model.machineRows.length, 65);
  assert.ok(model.machineRows.every(row => row.connected));
  assert.deepEqual(model.machines.selected, selected);
  assert.equal(model.navigatorScroll, 400);
});

test('Cancel retires Reload continuation instead of reopening Settings', () => {
  let model = { ...settings(), appearance: { ...settings().appearance, dirty: false } };
  [model] = step(model, { kind: 'settings_reload' });
  assert.equal(model.settingsReloadStage, 1);
  [model] = step(model, { kind: 'settings_close' });
  const [closed, cmd] = step(model, { kind: 'appearance_loaded', body: appearance(false) });
  assert.equal(closed.settingsOpen, false);
  assert.equal(closed.settingsReloadStage, 0);
  assert.equal(request(cmd, 'cockpit.appearance'), undefined);
});

test('late keybinding status cannot replace a Settings rollback with Begin', () => {
  const [closing] = step(settings(), { kind: 'settings_close' });
  const [waiting, cmd] = step(closing, { kind: 'keybindings_loaded', body: new Uint8Array([1, 0, 0, 0]) });
  assert.equal(waiting.appearanceClosing, true);
  assert.equal(waiting.appearanceBusy, true);
  assert.equal(request(cmd, 'cockpit.appearance'), undefined);
});

test('successful captured Browse Sessions receipt opens Sessions', () => {
  let [model] = step(initialModel()[0], { kind: 'machines_open' });
  [model] = step(model, { kind: 'machines_loaded', body: inventory(model.machines.requestId) });
  let browse;
  [model, browse] = step(model, { kind: 'machine_pick', target: model.machineRows[0].target });
  const captured = request(browse, 'cockpit.machines').payload.slice(2, 14);
  const [sessions, cmd] = step(model, { kind: 'machines_loaded', body: inventory(model.machines.requestId) });
  assert.equal(sessions.navigatorView, 1);
  assert.equal(sessions.paletteScope, 5);
  assert.deepEqual(sessions.paletteHost, captured);
  assert.deepEqual(request(cmd, 'cockpit.navigation').payload.slice(-14), new Uint8Array([5, 12, ...captured]));
  const wrong = captured.slice(); wrong[0] += 1;
  const staleHeader = navigationScopedRequest(sessions.engineRevision, 0, bytes(''), 5, wrong);
  const [ignored] = step(sessions, { kind: 'navigation_loaded', body: new Uint8Array([...staleHeader, 0, 0, 0, 0x4e]) });
  assert.equal(ignored.paletteLoading, true, 'a page for another machine capture cannot settle this request');
  assert.deepEqual(ignored.paletteHost, captured);
  const [filtered, filteredCmd] = step(sessions, { kind: 'palette_edit', edit: { kind: 'insert_text', text: bytes('work') } });
  assert.deepEqual(request(filteredCmd, 'cockpit.navigation').payload.slice(-12), captured);
  const [ordinary] = step(filtered, { kind: 'sessions_open' });
  assert.equal(ordinary.paletteScope, 1);
  assert.equal(ordinary.paletteHost.length, 0);
});

test('machine-scoped navigation requires and echoes the exact opaque twelve-byte capture', () => {
  const revision = { hi: 0, lo: 0 };
  const capture = new Uint8Array([...u32(7), ...u32(3), ...u32(9)]);
  const header = navigationScopedRequest(revision, 0, bytes(''), 5, capture);
  assert.deepEqual(header.slice(-14), new Uint8Array([5, 12, ...capture]));
  const page = new Uint8Array([...header, 0, 0, 0, 0x4e]);
  assert.deepEqual(navigationPage(page).host, capture);
  assert.equal(navigationScopedRequest(revision, 0, bytes(''), 5, bytes('This Mac')).length, 0);
  const wrongLength = page.slice(); wrongLength[14] = 11;
  assert.equal(navigationPage(wrongLength), null);
});

test('late Windows receipt cannot dismiss newer Commands', () => {
  const base = { ...initialModel()[0], paletteOpen: true, navigatorView: 3, windowActionPending: true, windowActionId: 9 };
  const [commands] = step(base, { kind: 'commands_open' });
  const receipt = new Uint8Array(27); receipt[0] = 1; receipt[1] = 1; receipt[3] = 9;
  const [after, cmd] = step(commands, { kind: 'window_action_loaded', body: receipt });
  assert.equal(after.paletteOpen, true);
  assert.equal(after.navigatorView, 4);
  assert.equal(cmd, null);
});

test('New Window and window closure retire the Settings transaction first', () => {
  for (const msg of [{ kind: 'new_window' }, { kind: 'window_closed', window: 1 }]) {
    const [waiting, cmd] = step(settings(), msg);
    assert.deepEqual([...request(cmd, 'cockpit.appearance').payload], [1, 6, 0]);
    assert.equal(request(cmd, 'cockpit.tab-command'), undefined);
    const [closed, departed] = step(waiting, { kind: 'appearance_loaded', body: appearance(false) });
    assert.equal(closed.settingsOpen, false);
    assert.ok(committed(departed), msg.kind);
    assert.ok(request(departed, msg.kind === 'new_window' ? 'cockpit.tab-command' : 'cockpit.snapshot'));
  }
});

test('departure before Describe waits for the opaque token and cancels before reopening', () => {
  let [model] = step(initialModel()[0], { kind: 'new_session_open' });
  let cmd;
  [model, cmd] = step(model, { kind: 'commands_open' });
  assert.equal(request(cmd, 'cockpit.new-session'), undefined, 'never send an invalid zero-token cancel');
  assert.equal(model.paletteOpen, false);
  [model, cmd] = step(model, { kind: 'new_session_loaded', body: session(0) });
  assert.deepEqual(request(cmd, 'cockpit.new-session').payload.slice(2, 10), token);
  assert.equal(request(cmd, 'cockpit.new-session').payload[1], 4);
  [model, cmd] = step(model, { kind: 'new_session_cancelled', body: session(0) });
  assert.equal(model.paletteOpen, true);
  assert.equal(model.navigatorView, 4);
  assert.equal(model.pendingSessionAction, null);
});

test('cancellation acknowledgement resumes only the latest destination', () => {
  let [model] = step(pendingSession(), { kind: 'commands_open' });
  [model] = step(model, { kind: 'new_session_open' });
  let cmd;
  [model, cmd] = step(model, { kind: 'new_session_cancelled', body: session(0) });
  assert.equal(request(cmd, 'cockpit.new-session').payload[1], 1);
  assert.equal(model.creatingSession, true);
  const [late] = step(model, { kind: 'new_session_loaded', body: session(2) });
  assert.equal(late.creatingSession, true);
  assert.equal(late.newSessionToken.length, 0);
});

test('Escape withdraws a destination waiting for New Session cancellation', () => {
  let [model] = step(pendingSession(), { kind: 'commands_open' });
  [model] = step(model, { kind: 'palette_close' });
  [model] = step(model, { kind: 'new_session_cancelled', body: session(0) });
  assert.equal(model.paletteOpen, false);
  assert.equal(model.pendingSessionAction, null);
});

test('failed or malformed cancellation never claims that focus authority was withdrawn', () => {
  for (const reply of [{ kind: 'new_session_cancel_failed', error: bytes('failed') }, { kind: 'new_session_cancelled', body: bytes('bad') }]) {
    const [pending] = step(pendingSession(), { kind: 'commands_open' });
    const [failed, cmd] = step(pending, reply);
    assert.equal(failed.paletteOpen, false);
    assert.equal(failed.renameOpen, true);
    assert.equal(failed.pendingSessionAction, null);
    assert.ok(committed(cmd));
    assert.equal(request(cmd, 'cockpit.keybindings'), undefined);
    const [, retry] = step(failed, { kind: 'rename_close' });
    assert.equal(request(retry, 'cockpit.new-session').payload[1], 4);
  }
});

test('malformed creation status clears awaiting and pre-Describe failure releases departure', () => {
  const [failed] = step(pendingSession(), { kind: 'new_session_loaded', body: bytes('bad') });
  assert.equal(failed.newSessionAwaiting, false);
  let [model] = step(initialModel()[0], { kind: 'new_session_open' });
  [model] = step(model, { kind: 'commands_open' });
  [model] = step(model, { kind: 'new_session_failed', error: bytes('failed') });
  assert.equal(model.navigatorView, 4);
  assert.equal(model.paletteOpen, true);
});

test('Escape cancels the Windows effect and late failure cannot overwrite newer Commands', () => {
  const base = { ...initialModel()[0], paletteOpen: true, navigatorView: 3, windowActionPending: true, windowActionId: 9 };
  const [closed, cmd] = step(base, { kind: 'palette_close' });
  assert.ok(effects(cmd).some(effect => effect.op === 'cancel' && effect.key === 'cockpit-window-command'));
  const [commands] = step(closed, { kind: 'commands_open' });
  const [late, ignored] = step(commands, { kind: 'window_action_failed', error: bytes('failed') });
  assert.deepEqual(late, commands);
  assert.equal(ignored, null);
});

test('a snapshot refreshes Connecting machines without clearing the list or scroll', () => {
  let [model] = step(initialModel()[0], { kind: 'machines_open' });
  [model] = step(model, { kind: 'machines_loaded', body: inventory(model.machines.requestId, 1) });
  model = { ...model, navigatorScroll: 64 };
  const snapshot = new Uint8Array(33);
  snapshot[0] = 1; snapshot[1] = 2; snapshot[10] = 7; snapshot[23] = 2; snapshot[26] = 168; snapshot[29] = 255;
  const [refreshing, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshot });
  assert.equal(request(cmd, 'cockpit.machines')?.payload[1], 1);
  assert.equal(refreshing.machineRows.length, 1);
  assert.equal(refreshing.navigatorScroll, 64);
  const [connected] = step(refreshing, { kind: 'machines_loaded', body: inventory(refreshing.machines.requestId, 2) });
  assert.equal(connected.machineRows.length, 1);
  assert.equal(connected.machineRows[0].connected, true);
});
