// Actual NAPI exports, actual runtime, isolated PTY-backed server. There is no
// JSON command shim and no terminal-cell query on this JS surface.
const assert = require('node:assert/strict');
const { Worker } = require('node:worker_threads');
const addonPath = process.argv[2];
const socketPath = process.argv[3];
const { DesktopClient, nativeClientStatus } = require(addonPath);
const client = new DesktopClient();
const events = [];
let closed = false;
let wakes = 0;

async function until(predicate, description) {
  const deadline = Date.now() + 20_000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, `timeout: ${description}; status=${client.status()}; error=${client.lastError()}`);
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

function answered(kind, requestId) {
  return events.find(event => event.kind === kind && event.requestId === requestId);
}

async function main() {
  const handle = client.handle;
  assert.match(handle, /^phux-client:[1-9][0-9]*$/);
  assert.throws(() => client.status(), /NotConnected/);
  assert.throws(() => client.connect({socketPath, cols: 0, rows: 24}, () => {}), /InvalidConnectOptions/);
  for (const value of [-1, 1.5, 65536, 4294967297, NaN, Infinity]) {
    assert.throws(() => client.connect({socketPath, cols: value, rows: 24}, () => {}), /InvalidConnectOptions/);
    assert.throws(() => client.connect({socketPath, cols: 80, rows: value}, () => {}), /InvalidConnectOptions/);
    assert.throws(() => client.status(), /NotConnected/, 'invalid dimensions must not start a runtime');
  }
  for (const sessionName of ['', 'x\0y', 'x'.repeat(4097)]) {
    assert.throws(() => client.connect({socketPath, cols: 80, rows: 24, sessionName}, () => {}), /InvalidSessionName/);
    assert.throws(() => client.status(), /NotConnected/);
  }
  client.connect({socketPath, cols: 80, rows: 24, sessionName: 'napi-smoke'}, deliveredHandle => {
    assert.equal(deliveredHandle, handle);
    // Already queued wakes may outlive close. Never use a stale identity.
    if (closed) return;
    wakes++;
    events.push(...client.takeEvents());
  });
  assert.throws(() => client.connect({socketPath, cols: 80, rows: 24}, () => {}), /AlreadyConnected/);
  await until(() => client.topology()?.sessions.some(session => session.name === 'napi-smoke'), 'topology');
  await until(() => client.status() === 'Attached', 'attach session');
  assert.equal(nativeClientStatus(handle), 'Attached', 'host painter seam shares the actual connected Client');
  const topology = client.topology();
  const pane = topology.panes.find(pane => pane.sessionName === 'napi-smoke');
  assert.ok(pane);
  assert.equal(typeof client.connectionEpoch(), 'string');
  assert.throws(() => client.attachTerminal('bad-id'), /InvalidResourceId/);
  await until(() => client.inputReadiness(pane.terminalId).ready, 'input ready');
  const deliveryId = client.applyPaste(pane.terminalId, '# napi real PTY input');
  assert.equal(typeof deliveryId, 'string');
  await until(() => events.some(event => event.kind === 'InputDelivery' && event.deliveryId === deliveryId), 'acknowledged input');
  const delivery = events.find(event => event.deliveryId === deliveryId);
  assert.equal(delivery.outcome, 'Delivered', JSON.stringify(delivery));
  const unsafeId = client.applyPaste(pane.terminalId, 'untrusted newline\n');
  await until(() => events.some(event => event.deliveryId === unsafeId), 'server paste refusal');
  assert.equal(events.find(event => event.deliveryId === unsafeId).outcome, 'Refused');
  const refusedId = client.applyPaste('bad-id', 'must not arrive');
  await until(() => events.some(event => event.deliveryId === refusedId), 'local refusal notification');
  assert.equal(events.find(event => event.deliveryId === refusedId).outcome, 'Refused');
  const detachId = client.detachTerminal(pane.terminalId);
  await until(() => answered('DetachAnswered', detachId), 'detach terminal');
  assert.equal(answered('DetachAnswered', detachId).error, undefined);
  const attachId = client.attachTerminal(pane.terminalId);
  await until(() => answered('AttachAnswered', attachId), 'reattach terminal');
  assert.equal(answered('AttachAnswered', attachId).error, undefined);
  const beforeInvalidSpawn = client.refreshTopology();
  for (const value of [-1, 4294967296, 4294967297, 1.5, NaN, Infinity]) {
    assert.throws(() => client.spawnTerminal(value), /InvalidSessionId/);
  }
  // No asynchronous JS turn in this interval: invalid inputs cannot consume
  // the next command correlation or issue a spawn to the server.
  assert.equal(client.refreshTopology(), beforeInvalidSpawn + 1);
  const spawnId = client.spawnTerminal(pane.sessionId);
  await until(() => answered('SpawnAnswered', spawnId), 'spawn terminal');
  assert.ok(answered('SpawnAnswered', spawnId).terminalId);
  assert.ok(wakes >= 3, 'later activity must rearm wake');
  assert.equal(typeof client.acquire, 'undefined', 'no grid export');
  closed = true;
  const undrained = client.applyPaste('bad-id', 'queued before disposal');
  const finalBatch = client.close();
  assert.equal(finalBatch.filter(event => event.kind === 'InputDelivery' && event.deliveryId === undrained && event.outcome === 'Refused').length, 1);
  assert.throws(() => client.takeEvents(), /StaleHandle/);
  assert.throws(() => client.status(), /StaleHandle/);
  assert.throws(() => nativeClientStatus(handle), /StaleHandle/);
  assert.throws(() => client.close(), /StaleHandle/);
  const replacement = new DesktopClient();
  assert.notEqual(replacement.handle, handle);
  replacement.close();
  // A Worker tears down a live environment without calling close. Its weak
  // TSFN must not hold the event loop open, and cleanup closes the runtime.
  const worker = new Worker(`
    const { DesktopClient } = require(${JSON.stringify(addonPath)});
    global.client = new DesktopClient();
    global.client.connect(${JSON.stringify({socketPath, cols: 80, rows: 24})}, () => {});
  `, { eval: true });
  await new Promise((resolve, reject) => {
    worker.once('error', reject);
    worker.once('exit', code => code === 0 ? resolve() : reject(new Error('worker exit ' + code)));
  });
  await require('./napi-cleanup.cjs')({ DesktopClient, nativeClientStatus, addonPath, socketPath });
  console.log(`NAPI smoke passed: topology, attach, acknowledged input, refusal, detach, reattach, spawn, stale handles, ${wakes} wakes, environment cleanup`);
}

main().catch(error => {
  console.error(error);
  closed = true;
  try { client.close(); } catch {}
  process.exitCode = 1;
});
