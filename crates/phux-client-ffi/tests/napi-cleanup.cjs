const assert = require('node:assert/strict');
const { Worker } = require('node:worker_threads');

async function collect() {
  for (let turn = 0; turn < 3; turn++) {
    global.gc();
    await new Promise(resolve => setImmediate(resolve));
  }
}

async function disposedChurn(DesktopClient, nativeClientStatus) {
  let abandoned = new DesktopClient();
  const abandonedHandle = abandoned.handle;
  abandoned = null;
  await collect();
  assert.throws(() => nativeClientStatus(abandonedHandle), /StaleHandle/, 'GC removes abandoned registry entries');
  const rss = [];
  for (let batch = 0; batch < 6; batch++) {
    for (let index = 0; index < 50_000; index++) {
      new DesktopClient().close();
    }
    await collect();
    rss.push(process.memoryUsage().rss);
  }
  // The old per-client hooks retained about 38 MB across these same counts.
  // The deterministic Rust test separately asserts exactly one registration.
  const growth = rss.at(-1) - rss[0];
  assert.ok(growth < 16 * 1024 * 1024, `disposed native RSS keeps growing: ${rss.join(', ')}`);
  console.log(`NAPI disposed churn: 50k RSS=${rss[0]}, 300k RSS=${rss.at(-1)}, growth=${growth} bytes`);
}

async function terminatedWorker(addonPath, socketPath) {
  const worker = new Worker(`
    const { parentPort } = require('node:worker_threads');
    const { DesktopClient } = require(${JSON.stringify(addonPath)});
    const client = new DesktopClient();
    let requested = false;
    let announced = false;
    client.connect(${JSON.stringify({ socketPath, cols: 80, rows: 24 })}, () => {
      client.takeEvents();
      if (!requested && client.topology()) {
        requested = true;
        client.attachSession('napi-smoke');
      }
      if (!announced && client.status() === 'Attached') {
        announced = true;
        parentPort.postMessage('attached');
      }
    });
    setInterval(() => {}, 1000);
  `, { eval: true });
  await new Promise((resolve, reject) => {
    worker.once('error', reject);
    worker.once('message', resolve);
    worker.once('exit', code => reject(new Error(`worker exited before attach: ${code}`)));
  });
  assert.equal(await worker.terminate(), 1, 'force-terminated an attached environment');
}

module.exports = async ({ DesktopClient, nativeClientStatus, addonPath, socketPath }) => {
  assert.equal(typeof global.gc, 'function', 'run fixture with --expose-gc');
  await disposedChurn(DesktopClient, nativeClientStatus);
  for (let index = 0; index < 100; index++) {
    await terminatedWorker(addonPath, socketPath);
  }
  console.log('NAPI cleanup passed: abandoned-object GC, 300k disposals, 100 forced Worker terminations');
};
