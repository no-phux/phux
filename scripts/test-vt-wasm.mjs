import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const engine = readFileSync(new URL('../clients/phux-vt-web/vendor/ghostty-vt.wasm', import.meta.url));
const prepare = fileURLToPath(new URL('./prepare-vt-wasm.mjs', import.meta.url));
test('metadata removal is deterministic, idempotent, and preserves the frozen engine ABI', () => {
  const scratch = mkdtempSync(join(tmpdir(), 'phux-wasm-test-'));
  try {
    for (const name of ['host-one', 'host-two']) {
      const path = join(scratch, `${name}.wasm`);
      const metadata = Buffer.from(name);
      // A valid custom section with a different host-specific name.
      const custom = Buffer.concat([Buffer.from([0, metadata.length + 1, metadata.length]), metadata]);
      writeFileSync(path, Buffer.concat([engine, custom]));
      for (let pass = 0; pass < 2; pass++) {
        const result = spawnSync(process.execPath, [prepare, path], { encoding: 'utf8' });
        assert.equal(result.status, 0, result.stderr);
        assert.deepEqual(readFileSync(path), engine);
      }
    }
    const bad = join(scratch, 'bad.wasm');
    writeFileSync(bad, 'corrupt');
    assert.notEqual(spawnSync(process.execPath, [prepare, bad]).status, 0);
    assert.equal(readFileSync(bad, 'utf8'), 'corrupt');
    // Valid WASM with no consumer ABI must also fail without rewriting it.
    const incompatible = Buffer.from([0, 97, 115, 109, 1, 0, 0, 0, 0, 2, 1, 120]);
    writeFileSync(bad, incompatible);
    assert.notEqual(spawnSync(process.execPath, [prepare, bad]).status, 0);
    assert.deepEqual(readFileSync(bad), incompatible);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});
