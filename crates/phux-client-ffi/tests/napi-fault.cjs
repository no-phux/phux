const assert = require('node:assert/strict');
const fs = require('node:fs');
const [addon, socketPath, marker, mode] = process.argv.slice(2);
const { DesktopClient, NativeClientLease, nativeClientStatus } = require(addon);
const client = new DesktopClient();
const handle = client.handle;
const events = [];
let closed = false;
let wakes = 0;

async function until(predicate, description) {
  const deadline = Date.now() + 20_000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, `timeout ${description}: ${client.lastError()}`);
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

async function main() {
  client.connect({ socketPath, cols: 80, rows: 24 }, received => {
    assert.equal(received, handle);
    if (closed) return;
    wakes++;
    events.push(...client.takeEvents());
  });
  await until(() => client.topology()?.sessions.some(session => session.name === 'napi-smoke'), 'initial topology');
  client.attachSession('napi-smoke');
  await until(() => client.status() === 'Attached', 'initial attach');
  const lease = new NativeClientLease(handle);
  const epoch = BigInt(client.connectionEpoch());
  const terminal = client.topology().panes.find(pane => pane.sessionName === 'napi-smoke').terminalId;
  await until(() => client.inputReadiness(terminal).ready, 'input ready');
  const deliveryId = client.applyPaste(terminal, '# withheld acknowledged input');
  await until(() => fs.existsSync(marker), 'proxy intercepted actual APPLY_INPUT');
  if (mode === 'restart') {
    const before = wakes;
    await until(() => BigInt(client.connectionEpoch()) > epoch && client.status() === 'Attached', 'new server incarnation');
    await until(() => events.some(event => event.kind === 'InputDelivery' && event.deliveryId === deliveryId), 'unknown outcome after incarnation change');
    assert.ok(wakes > before, 'reconnect must rearm notifications');
    assert.equal(client.handle, handle);
    assert.equal(lease.connectionEpoch(), client.connectionEpoch(), 'held native lease sees the same advanced epoch');
    assert.equal(lease.status(), 'Attached');
    assert.equal(nativeClientStatus(handle), 'Attached');
    // The binder never retries Unknown. Refreshing metadata must not emit a
    // second delivery result or clear the native delivery fence.
    assert.equal(client.inputReadiness(terminal).deliveryFenced, true);
    client.refreshTopology();
  }
  closed = true;
  events.push(...client.close());
  const outcomes = events.filter(event => event.kind === 'InputDelivery' && event.deliveryId === deliveryId);
  assert.equal(outcomes.length, 1, 'pending input resolves exactly once across drain and close');
  assert.equal(outcomes[0].outcome, 'Unknown', JSON.stringify(outcomes));
  assert.equal(lease.status(), 'Closed', 'held native lease observes close');
  assert.throws(() => nativeClientStatus(handle), /StaleHandle/);
  assert.throws(() => client.takeEvents(), /StaleHandle/);
  assert.throws(() => client.close(), /StaleHandle/);
  console.log(`NAPI ${mode} fault passed: exactly one Unknown, immediate stale handle, held native lease epoch ${epoch}->${lease.connectionEpoch()}, ${wakes} wakes`);
}

main().catch(error => {
  console.error(error);
  closed = true;
  try { client.close(); } catch {}
  process.exitCode = 1;
});
