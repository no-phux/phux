// SDK dependencies remain private, including the npm download cache. Only
// reviewed host compiler assets execute; no provider CLI or install hook runs.
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { installPinnedAsset } from './assets.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const cache = await mkdtemp(join(tmpdir(), 'phux-private-sdk-source-'));
const environment = { ...process.env, npm_config_cache: cache, npm_config_audit: 'false', npm_config_fund: 'false' };
for (const key of Object.keys(environment)) if (/^(PHUX_|NODE_AUTH_TOKEN$|NPM_TOKEN$)/.test(key)) delete environment[key];
function run(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd: root, env: environment, stdio: 'inherit', detached: false });
    child.once('error', reject);
    child.once('exit', (code, signal) => code === 0 ? resolve() : reject(new Error(`${command} failed (${signal ?? code})`)));
  });
}
try {
  await run('npm', ['ci', '--ignore-scripts', '--omit=optional', '--no-audit', '--no-fund']);
  const policy = JSON.parse(await readFile(join(root, 'runtime-policy.json'), 'utf8'));
  const lock = JSON.parse(await readFile(join(root, 'package-lock.json'), 'utf8'));
  const platform = `${process.platform}-${process.arch}`;
  for (const tool of ['esbuild', 'typescript_tools']) {
    await installPinnedAsset(policy.optional_policy[tool].packages[platform], { lock, policy, root, run });
  }
  await run('npm', ['run', 'typecheck']);
  await run('npm', ['test']);
  const before = await readFile(join(root, 'index.js'));
  await run('npm', ['run', 'generate']);
  const after = await readFile(join(root, 'index.js'));
  if (!before.equals(after)) throw new Error('Native SDK own-code bundle is stale; generate it and commit the result');
} finally { await rm(cache, { recursive: true, force: true }); }
