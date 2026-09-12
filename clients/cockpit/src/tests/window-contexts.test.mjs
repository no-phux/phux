import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update } from '../core.ts';
import { snapshot, windowContexts, validUtf8, MAX_WINDOWS } from '../protocol.ts';

const bytes = value => new TextEncoder().encode(value);
const text = value => new TextDecoder().decode(value);
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};

/// One version 1 record: `window, flags, connection, session, host`.
function entry({ window, flags = 0, connection = 2, session = '', host = '' }) {
  const s = typeof session === 'string' ? bytes(session) : Uint8Array.from(session);
  const h = typeof host === 'string' ? bytes(host) : Uint8Array.from(host);
  return [window, flags, connection, s.length, ...s, h.length, ...h];
}
function payload(entries, { version = 1, count = entries.length } = {}) {
  return new Uint8Array([version, count, ...entries.flatMap(entry)]);
}
function record(kind, body) {
  return [kind, body.length % 256, Math.floor(body.length / 256), ...body];
}

/// A snapshot with no tabs or themes, `secondary` open secondary slots, a
/// kind 3 navigation context, then each extension record given.
function snapshotBytes({ secondary = [], connection = 2, extensions = [] } = {}) {
  const head = new Uint8Array(28);
  head[0] = 1; head[1] = 2; head[10] = 7; head[23] = connection; head[26] = 168;
  const sections = secondary.flatMap(index => [index, 0, 0, 0, 0, 168, 0]);
  return new Uint8Array([...head, 0, 255, 0, 0, secondary.length, ...sections, 0, 0, 0, 0, 0, ...extensions.flat()]);
}
function navigation(session, endpoint) {
  const s = bytes(session);
  const e = bytes(endpoint);
  return record(3, [s.length, ...s, e.length, ...e, 0]);
}
function contextsRecord(entries, options) {
  return record(5, [...payload(entries, options)]);
}
function empty(windows, flags, name, host) {
  const n = bytes(name);
  const h = bytes(host);
  return record(4, [windows, flags, n.length, ...n, h.length, ...h]);
}
function loaded(options) {
  return step(initialModel()[0], { kind: 'snapshot_loaded', body: snapshotBytes(options) })[0];
}

// ---------------------------------------------------------------- decoder

test('a version 1 record decodes one, two and five windows exactly', () => {
  const one = windowContexts(payload([{ window: 0, connection: 0, session: 'Local terminals', host: 'This Mac' }]));
  assert.equal(one.length, 1);
  assert.equal(one[0].window, 0);
  assert.equal(one[0].connection, 0);
  assert.equal(text(one[0].session), 'Local terminals');
  assert.equal(text(one[0].host), 'This Mac');
  assert.deepEqual([one[0].empty, one[0].picked, one[0].opening, one[0].unavailable], [false, false, false, false]);

  const two = windowContexts(payload([
    { window: 0, session: 'alpha', host: 'studio' },
    { window: 3, flags: 0b1111, connection: 4, session: 'beta', host: 'mini' },
  ]));
  assert.deepEqual(two.map(one => one.window), [0, 3]);
  assert.deepEqual([two[1].empty, two[1].picked, two[1].opening, two[1].unavailable], [true, true, true, true]);
  assert.equal(two[1].connection, 4);

  const five = windowContexts(payload([4, 2, 0, 1, 3].map(window => ({ window, flags: window === 2 ? 5 : 0, connection: window % 5, session: `s${window}`, host: `h${window}` }))));
  assert.equal(five.length, MAX_WINDOWS);
  assert.deepEqual(five.map(one => text(one.session)), ['s4', 's2', 's0', 's1', 's3']);
  assert.equal(five[1].empty && five[1].opening && !five[1].picked, true);

  // A current engine always writes the record, even with nothing to say.
  assert.deepEqual(windowContexts(payload([])), []);
});

test('each strictness violation refuses the whole record', () => {
  const good = [{ window: 0, session: 'alpha', host: 'studio' }, { window: 1, session: 'beta', host: 'mini' }];
  assert.notEqual(windowContexts(payload(good)), null);
  const refused = {
    'unknown version': payload(good, { version: 2 }),
    'version 0': payload(good, { version: 0 }),
    'count above the payload': payload(good, { count: 3 }),
    'count below the payload': payload(good, { count: 1 }),
    'count above max windows': payload(good, { count: 6 }),
    'duplicate window': payload([good[0], { ...good[1], window: 0 }]),
    'window index = max windows': payload([{ window: MAX_WINDOWS }]),
    'window index 255': payload([{ window: 255 }]),
    'reserved flag bit 4': payload([{ window: 0, flags: 0x10 }]),
    'reserved flag bit 7': payload([{ window: 0, flags: 0x81 }]),
    'unknown connection': payload([{ window: 0, connection: 5 }]),
    'session over 64 bytes': payload([{ window: 0, session: 'x'.repeat(65) }]),
    'host over 64 bytes': payload([{ window: 0, host: 'y'.repeat(65) }]),
    'no count': new Uint8Array([1]),
    'empty payload': new Uint8Array(0),
  };
  for (const [name, body] of Object.entries(refused)) assert.equal(windowContexts(body), null, name);
  // Torn at every byte short of the end.
  const whole = payload(good);
  for (let end = 0; end < whole.length; end += 1) assert.equal(windowContexts(whole.subarray(0, end)), null, `torn at ${end}`);
  // Trailing bytes after the counted records.
  assert.equal(windowContexts(new Uint8Array([...whole, 0])), null);
});

test('strings are exactly 64 bytes of well-formed UTF-8 at most', () => {
  const at64 = 'é'.repeat(32); // 64 bytes of two-byte scalars
  const decoded = windowContexts(payload([{ window: 0, session: at64, host: '\u{1F5A5}'.repeat(16) }]));
  assert.equal(text(decoded[0].session), at64);
  assert.equal(decoded[0].host.length, 64);
  const malformed = {
    'lone continuation': [0x80],
    'cut two-byte': [0x61, 0xc3],
    'cut four-byte': [0xf0, 0x9f, 0x96],
    'overlong two-byte': [0xc0, 0xaf],
    'overlong three-byte': [0xe0, 0x80, 0xaf],
    'overlong four-byte': [0xf0, 0x80, 0x80, 0xaf],
    surrogate: [0xed, 0xa0, 0x80],
    'past U+10FFFF': [0xf4, 0x90, 0x80, 0x80],
    'lead F5': [0xf5, 0x80, 0x80, 0x80],
    'bad continuation': [0xe2, 0x82, 0x41],
  };
  for (const [name, raw] of Object.entries(malformed)) {
    assert.equal(validUtf8(Uint8Array.from(raw)), false, name);
    assert.equal(windowContexts(payload([{ window: 0, session: raw }])), null, `session ${name}`);
    assert.equal(windowContexts(payload([{ window: 0, host: raw }])), null, `host ${name}`);
  }
  for (const raw of [[], [0x7f], [0xc2, 0x80], [0xed, 0x9f, 0xbf], [0xee, 0x80, 0x80], [0xf4, 0x8f, 0xbf, 0xbf]]) {
    assert.equal(validUtf8(Uint8Array.from(raw)), true, raw.join(','));
  }
});

test('the snapshot exposes the record, refuses a malformed or repeated one, and marks its absence', () => {
  const good = contextsRecord([{ window: 0, session: 'alpha', host: 'studio' }]);
  const decoded = snapshot(snapshotBytes({ extensions: [navigation('primary', 'host'), good] }));
  assert.equal(decoded.windowContexts.present, true);
  assert.equal(text(decoded.windowContexts.records[0].session), 'alpha');
  // An older engine: no record at all.
  assert.equal(snapshot(snapshotBytes({ extensions: [navigation('primary', 'host')] })).windowContexts.present, false);
  // Refused like every known record: the snapshot is not partially applied.
  assert.equal(snapshot(snapshotBytes({ extensions: [contextsRecord([{ window: 0, flags: 0x20 }])] })), null);
  assert.equal(snapshot(snapshotBytes({ extensions: [good, good] })), null, 'two answers for one window');
  // Unknown kinds are still stepped over by length, before and after.
  const framed = snapshot(snapshotBytes({ extensions: [record(9, [1, 2, 3]), good, record(200, [])] }));
  assert.equal(framed.windowContexts.records.length, 1);
});

// ---------------------------------------------------------------- binding

test('two windows on different machines each carry their own header and state', () => {
  const model = loaded({ secondary: [1], extensions: [navigation('primary-global', 'global-host'), contextsRecord([
    { window: 0, connection: 2, session: 'alpha', host: 'studio' },
    { window: 1, connection: 3, session: 'beta', host: 'mini' },
  ])] });
  assert.equal(text(model.mainContext.title), 'alpha');
  assert.equal(text(model.mainContext.detail), 'studio');
  assert.equal(text(model.window1Context.title), 'beta');
  assert.equal(text(model.window1Context.detail), 'mini · Offline');
  assert.equal(text(model.connectionStatus), 'Phux connected');
  assert.equal(text(model.window1Status), 'Phux offline');
  // Closed windows are blank, never a copy of an open one.
  assert.equal(model.window2Context.title.length, 0);
});

test('an open window without a record is explicitly unknown, never the primary', () => {
  const model = loaded({ secondary: [1, 2], extensions: [navigation('primary-global', 'global-host'), contextsRecord([
    { window: 0, session: 'alpha', host: 'studio' },
  ])] });
  assert.equal(text(model.window1Context.title), 'Session unknown');
  assert.equal(text(model.window1Context.detail), 'Window context unavailable');
  assert.equal(text(model.window1Status), 'Connection status unavailable');
  assert.equal(text(model.window2Context.title), 'Session unknown');
  // Count 0 leaves even the primary unknown rather than borrowing kind 3.
  const none = loaded({ extensions: [navigation('primary-global', 'global-host'), contextsRecord([])] });
  assert.equal(text(none.mainContext.title), 'Session unknown');
  assert.equal(text(none.connectionStatus), 'Connection status unavailable');
});

test('state words: unavailable, offline, connecting, opening and empty each reach their own window', () => {
  const cases = [
    [{ flags: 8, connection: 2 }, 'mini · Unavailable'],
    [{ connection: 4 }, 'mini · Workspace unavailable'],
    [{ connection: 1 }, 'mini · Connecting...'],
    [{ flags: 5, connection: 2 }, 'mini · Opening...'],
    [{ flags: 1, connection: 2 }, 'mini · Empty session'],
    [{ connection: 2 }, 'mini'],
    [{ connection: 0, host: 'This Mac' }, 'This Mac'],
  ];
  for (const [fields, detail] of cases) {
    const model = loaded({ secondary: [2], extensions: [contextsRecord([
      { window: 0, session: 'alpha', host: 'studio' },
      { window: 2, session: 'beta', host: 'mini', ...fields },
    ])] });
    assert.equal(text(model.window2Context.detail), detail, JSON.stringify(fields));
    assert.equal(text(model.mainContext.detail), 'studio', 'the primary keeps its own');
  }
});

test('the Empty session view uses each window its own name, host, picked and opening state', () => {
  const model = loaded({ secondary: [1, 3], extensions: [empty(1, 0, 'legacy', 'nowhere'), contextsRecord([
    { window: 0, session: 'alpha', host: 'studio' },
    { window: 1, flags: 1 | 2, session: 'scratch', host: 'mini' },
    { window: 3, flags: 1 | 4, session: 'other', host: 'lab' },
  ])] });
  // Kind 5 wins over kind 4 for every window, including the mask.
  assert.equal(model.emptyWindows, 2 | 8);
  assert.equal(model.mainEmptyOpen, false);
  assert.equal(model.window1EmptyOpen, true);
  assert.equal(model.window3EmptyOpen, true);
  assert.equal(text(model.window1Context.emptyName), 'scratch');
  assert.equal(text(model.window1Context.emptyDetail), 'Empty session on mini');
  assert.equal(model.window1Context.emptyPicked, true);
  assert.equal(model.window1Context.emptyOpening, false);
  assert.equal(text(model.window3Context.emptyName), 'other');
  assert.equal(text(model.window3Context.emptyDetail), 'Empty session on lab');
  assert.equal(model.window3Context.emptyPicked, false);
  assert.equal(model.window3Context.emptyOpening, true);
  // The active primary is not opening, so its New Tab gate stays open.
  assert.equal(model.emptyBusy, false);
  assert.equal(model.emptyPicked, true);
});

test('old snapshots without kind 5 keep the kind 3 and kind 4 labels', () => {
  const model = loaded({ secondary: [1], extensions: [navigation('primary-global', 'global-host'), empty(2, 1, 'scratch', 'mini')] });
  for (const context of [model.mainContext, model.window1Context]) {
    assert.equal(text(context.title), 'primary-global');
    assert.equal(text(context.detail), 'global-host');
  }
  assert.equal(model.emptyWindows, 2);
  assert.equal(text(model.window1Context.emptyName), 'scratch');
  assert.equal(text(model.window1Context.emptyDetail), 'Empty session on mini');
  assert.equal(text(model.window1Status), 'Phux connected');
  const bare = loaded();
  assert.equal(text(bare.mainContext.title), 'Sessions');
  assert.equal(text(bare.mainContext.detail), 'Machine not yet known');
});

test('closing a window clears its context on the next snapshot', () => {
  let model = loaded({ secondary: [1], extensions: [contextsRecord([
    { window: 0, session: 'alpha', host: 'studio' },
    { window: 1, flags: 1, session: 'beta', host: 'mini' },
  ])] });
  assert.equal(text(model.window1Context.title), 'beta');
  [model] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes({ extensions: [contextsRecord([{ window: 0, session: 'alpha', host: 'studio' }])] }) });
  assert.equal(model.window1Open, false);
  assert.equal(model.window1Context.title.length, 0);
  assert.equal(model.window1Context.emptyName.length, 0);
  assert.equal(model.window1EmptyOpen, false);
  assert.equal(model.emptyWindows, 0);
});

test('every window template binds its own context, not the ambient primary labels', () => {
  const markup = readFileSync(new URL('../windows/components/cockpit-window.native', import.meta.url), 'utf8');
  const header = markup.slice(markup.indexOf('<list-item height="40" width="176" label="Sessions"'), markup.indexOf('</list-item>'));
  assert.match(header, /\{title\}/);
  assert.match(header, /\{detail\}/);
  assert.doesNotMatch(header, /workspaceLabel|machineLabel/);
  const uses = [['../app.native', 'main'], ...[1, 2, 3, 4].map(n => [`../windows/phux-window-${n}.native`, `window${n}`])];
  for (const [file, prefix] of uses) {
    const source = readFileSync(new URL(file, import.meta.url), 'utf8');
    assert.match(source, new RegExp(`title="\\{${prefix}Context\\.title\\}" detail="\\{${prefix}Context\\.detail\\}"`), file);
    assert.match(source, new RegExp(`name="\\{${prefix}Context\\.emptyName\\}" detail="\\{${prefix}Context\\.emptyDetail\\}"`), file);
  }
});
