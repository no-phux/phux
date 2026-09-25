// The Rust fixture owns both daemon incarnations, all PTYs and this directory.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const [addon, socketPath, directory] = process.argv.slice(2);
const { DesktopClient, NativeClientLease, nativeClientStatus } = require(addon);
const production = process.argv.includes('--production-host');
const client = new DesktopClient();
const events = [];
let closed = false;

async function until(predicate, description) {
  const deadline = Date.now() + 20_000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, `timeout ${description}: ${client.lastError()}`);
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

const answer = (kind, requestId) => events.find(event => event.kind === kind && event.requestId === requestId);

function rejectInvalidSpawns(base) {
  const before = client.refreshTopology();
  for (const sessionId of [-1, 4294967296, 4294967297, 1.5, NaN, Infinity]) {
    assert.throws(() => client.spawnTerminalWithOptions({ ...base, sessionId }), /InvalidSessionId/);
  }
  assert.throws(() => client.spawnTerminalWithOptions({ ...base, sessionId: 4294967295 }), /SessionNotFound/);
  for (const command of [[], [''], Array(257).fill('x')]) {
    assert.throws(() => client.spawnTerminalWithOptions({ ...base, command }), /InvalidSpawnCommand/);
  }
  for (const command of [['x\0y'], ['x'.repeat(65537)]]) {
    assert.throws(() => client.spawnTerminalWithOptions({ ...base, command }), /InvalidSpawnText/);
  }
  for (const env of [[{ name: '', value: 'x' }], [{ name: 'A=B', value: 'x' }],
    [{ name: 'X', value: '1' }, { name: 'X', value: '2' }]]) {
    assert.throws(() => client.spawnTerminalWithOptions({ ...base, env }), /InvalidSpawnEnvironment/);
  }
  assert.throws(() => client.spawnTerminalWithOptions({ ...base, cwd: '' }), /InvalidSpawnDirectory/);
  assert.throws(() => client.spawnTerminalWithOptions({ ...base, env: [{ name: 'X', value: '\0' }] }), /InvalidSpawnText/);
  assert.throws(() => client.spawnTerminalWithOptions({ ...base, initialSize: { cols: 1.5, rows: 20 } }), /InvalidConnectOptions/);
  assert.equal(client.refreshTopology(), before + 1, 'invalid input never allocates a command ID');
}

async function lifecycle(identity, seed) {
  const base = { identity, sessionId: seed.sessionId };
  rejectInvalidSpawns(base);
  const beforeSatellite = client.refreshTopology();
  assert.throws(() => client.terminateTerminal(identity, 'satellite:remote:7'), /UnsupportedSatelliteTermination/);
  assert.equal(client.refreshTopology(), beforeSatellite + 1, 'satellite rejection allocates no command');
  const reportPath = path.join(directory, 'child.json');
  const injectionPath = path.join(directory, 'must-not-exist');
  const args = ['space here', '', 'quote" single\' unicode 😀', `$(touch ${injectionPath})`, '; exit 42', 'line\nfeed'];
  const value = 'spaces = quotes "\' 😀\nnewline';
  const program = `require('node:fs').writeFileSync(${JSON.stringify(reportPath)}, JSON.stringify({
    pid: process.pid, args: process.argv.slice(1), cwd: process.cwd(), env: process.env.PHUX_NAPI_VALUE,
    cols: process.stdout.columns, rows: process.stdout.rows
  })); setInterval(() => {}, 1000);`;
  const requestId = client.spawnTerminalWithOptions({ ...base,
    command: [process.execPath, '-e', program, ...args], cwd: directory,
    env: [{ name: 'PHUX_NAPI_VALUE', value }], initialSize: { cols: 93, rows: 31 }
  });
  await until(() => answer('SpawnAnswered', requestId), 'spawn acknowledgement');
  const spawned = answer('SpawnAnswered', requestId);
  assert.equal(spawned.error, undefined);
  assert.ok(spawned.terminalId);
  assert.notEqual(spawned.terminalId, seed.terminalId);
  await until(() => fs.existsSync(reportPath), 'spawned process report');
  const report = JSON.parse(fs.readFileSync(reportPath, 'utf8'));
  assert.deepEqual(report.args, args, 'argv preserves boundaries and empty/literal shell arguments');
  assert.equal(report.cwd, fs.realpathSync(directory));
  assert.equal(report.env, value);
  assert.deepEqual([report.cols, report.rows], [93, 31]);
  assert.equal(fs.existsSync(injectionPath), false, 'no shell evaluation');
  const detached = client.detachTerminal(spawned.terminalId);
  await until(() => answer('DetachAnswered', detached), 'detach acknowledgement');
  assert.equal(answer('DetachAnswered', detached).error, undefined);
  assert.doesNotThrow(() => process.kill(report.pid, 0), 'detach leaves the process alive');
  client.refreshTopology();
  await until(() => client.topology()?.panes.some(p => p.terminalId === spawned.terminalId), 'spawn topology');
  const killed = client.terminateTerminal(identity, spawned.terminalId);
  await until(() => answer('TerminalKilled', killed), 'typed kill acknowledgement');
  assert.equal(answer('TerminalKilled', killed).terminalId, spawned.terminalId);
  assert.equal(answer('TerminalKilled', killed).error, undefined);
  await until(() => events.some(e => e.kind === 'Closed' && e.terminalId === spawned.terminalId), 'authoritative resource close');
  assert.equal(events.filter(e => e.kind === 'TerminalKilled' && e.requestId === killed).length, 1);
  assert.ok(!events.some(e => e.kind === 'Closed' && e.terminalId === seed.terminalId), 'seed was not killed');
  await until(() => { try { process.kill(report.pid, 0); return false; } catch (e) { return e.code === 'ESRCH'; } }, 'only owned process terminated');
}

async function restart(identity, lease) {
  fs.writeFileSync(path.join(directory, 'restart'), 'ready');
  await until(() => client.status() === 'Attached' && BigInt(client.connectionEpoch()) > BigInt(identity.connectionEpoch), 'daemon restart');
  const current = client.serverInfo();
  assert.notEqual(current.serverId, identity.serverId, 'daemon incarnation differs, not merely transport epoch');
  if (production) assert.equal(nativeClientStatus(client.handle), 'Attached');
  else assert.equal(current.connectionEpoch, lease.connectionEpoch(), 'held native Client shares reconnect');
  const pane = client.topology().panes.find(p => p.sessionName === 'napi-smoke');
  assert.throws(() => client.terminateTerminal(identity, pane.terminalId), /StaleConnectionIdentity/);
  assert.throws(() => client.spawnTerminalWithOptions({ identity, sessionId: pane.sessionId }), /StaleConnectionIdentity/);
  assert.throws(() => client.terminateTerminal({ ...current, serverId: identity.serverId }, pane.terminalId), /StaleConnectionIdentity/);
  assert.throws(() => client.terminateTerminal({ ...current, connectionEpoch: identity.connectionEpoch }, pane.terminalId), /StaleConnectionIdentity/);
  // A fresh client has epoch 1 again, but its incarnation is the new daemon.
  const fresh = new DesktopClient();
  let disposed = false;
  try {
    fresh.connect({ socketPath, cols: 80, rows: 24, sessionName: 'napi-smoke' }, () => { if (!disposed) fresh.takeEvents(); });
    await until(() => fresh.serverInfo(), 'fresh connection negotiation');
    assert.equal(fresh.serverInfo().connectionEpoch, identity.connectionEpoch);
    assert.equal(fresh.serverInfo().serverId, current.serverId);
    assert.notEqual(fresh.serverInfo().serverId, identity.serverId);
    await until(() => fresh.topology()?.sessions.some(s => s.name === 'napi-smoke'), 'fresh topology');
    await until(() => fresh.status() === 'Attached', 'fresh attach');
    const freshPane = fresh.topology().panes.find(p => p.sessionName === 'napi-smoke');
    const before = fresh.refreshTopology();
    assert.throws(() => fresh.spawnTerminalWithOptions({ identity, sessionId: freshPane.sessionId }), /StaleConnectionIdentity/);
    assert.throws(() => fresh.terminateTerminal(identity, freshPane.terminalId), /StaleConnectionIdentity/);
    assert.equal(fresh.refreshTopology(), before + 1, 'same-epoch old-incarnation rejection allocates no command');
  } finally { disposed = true; fresh.close(); }
}

async function main() {
  client.connect({ socketPath, cols: 80, rows: 24, sessionName: 'napi-smoke' }, () => { if (!closed) events.push(...client.takeEvents()); });
  await until(() => client.topology()?.sessions.some(s => s.name === 'napi-smoke'), 'initial topology');
  const negotiated = client.serverInfo();
  assert.match(negotiated.serverId, /^(?:[0-9a-f]{2})+$/);
  assert.match(negotiated.protocol, /^\d+\.\d+\.\d+$/);
  assert.ok(['NativeState', 'SynthesizedVtRaw', 'SynthesizedVtStateSync'].includes(negotiated.profile.kind));
  assert.ok(negotiated.features.includes('acknowledged-input'));
  assert.ok(negotiated.featureBits > 0 && negotiated.layerBits > 0);
  assert.ok(negotiated.maxChunkBytes > 0 && negotiated.maxHistoryPageBytes > 0);
  await until(() => client.status() === 'Attached', 'initial attach');
  const identity = client.serverInfo();
  const lease = production ? undefined : new NativeClientLease(client.handle);
  const seed = client.topology().panes.find(p => p.sessionName === 'napi-smoke');
  await lifecycle(identity, seed);
  await restart(identity, lease);
  closed = true;
  events.push(...client.close());
  assert.throws(() => client.serverInfo(), /StaleHandle/);
  console.log('NAPI connection smoke passed: lossless negotiation, qualified lifecycle, exact argv/cwd/env/size, detach vs owned kill, actual daemon incarnation restart');
}

main().catch(error => {
  console.error(error);
  closed = true;
  try { client.close(); } catch {}
  process.exitCode = 1;
});
