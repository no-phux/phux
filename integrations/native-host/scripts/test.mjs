import { build } from 'esbuild';
import { mkdtemp, readdir, rm } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const directory = await mkdtemp(join(root, '.module-test-'));
try {
  const tests = (await readdir(join(root, 'test'))).filter(file => file.endsWith('.test.ts'));
  await build({ absWorkingDir: root, entryPoints: tests.map(file => join('test', file)), outdir: directory, bundle: true, packages: 'external', platform: 'node', target: 'node22.19', format: 'esm', outExtension: { '.js': '.mjs' } });
  const child = spawn(process.execPath, ['--test', ...tests.map(file => join(directory, file.replace(/\.ts$/, '.mjs')))], { cwd: root, stdio: 'inherit', detached: false });
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', (code, signal) => resolve(signal ? 1 : code ?? 1)); });
  process.exitCode = code;
} finally {
  await rm(directory, { recursive: true, force: true });
}
