import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile, copyFile, chmod, rename, rm, stat } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { join } from 'node:path';

import { installPinnedAsset } from './assets.mjs';
const [destination, manifestPath, lockPath, policyPath, bridgePath, ...consent] = process.argv.slice(2);
if (!destination || !manifestPath || !lockPath || !policyPath || !bridgePath) throw new Error('SDK setup requires an explicit private destination and pinned artifacts');
const [major, minor] = process.versions.node.split('.').map(Number);
if (major < 22 || (major === 22 && minor < 19)) throw new Error('Native SDK runtime requires Node >=22.19');
const policy = JSON.parse(await readFile(policyPath, 'utf8'));
const lock = JSON.parse(await readFile(lockPath, 'utf8'));
if (policy.schema !== 'phux.native-sdk-runtime/v1') throw new Error('Unsupported SDK runtime policy');
if (!consent.includes('--accept-claude-terms') || !consent.includes('--accept-amp-terms')) {
  throw new Error('Explicit acceptance required: Claude SDK https://www.anthropic.com/legal/commercial-terms and Amp SDK https://ampcode.com/terms; rerun with --accept-claude-terms --accept-amp-terms only after reviewing those terms');
}
const platform = `${process.platform}-${process.arch}`;
const staging = `${destination}.install-${process.pid}`;
await mkdir(staging, { recursive: false, mode: 0o700 });
await writeFile(join(staging, '.npmrc.global'), '', { mode: 0o600 });
const cache = join(staging, '.npm-cache');
const env = { ...process.env, npm_config_cache: cache, npm_config_userconfig: join(staging, '.npmrc'), npm_config_globalconfig: join(staging, '.npmrc.global') };
for (const key of Object.keys(env)) if (/^(PHUX_|NODE_AUTH_TOKEN$|NPM_TOKEN$|npm_config_(registry|auth|token))/i.test(key)) delete env[key];
function run(command, args, cwd = staging) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, env, stdio: ['ignore', 2, 2] });
    child.once('error', reject);
    child.once('exit', (code, signal) => code === 0 ? resolve() : reject(new Error(`${command} failed (${signal ?? code})`)));
  });
}
function installOptional(name) {
  return installPinnedAsset(name, { lock, policy, root: staging, run });
}
try {
  await copyFile(manifestPath, join(staging, 'package.json'));
  await copyFile(lockPath, join(staging, 'package-lock.json'));
  await copyFile(policyPath, join(staging, 'runtime-policy.json'));
  await copyFile(bridgePath, join(staging, 'index.mjs'));
  await writeFile(join(staging, '.npmrc'), 'registry=https://registry.npmjs.org/\nignore-scripts=true\naudit=false\nfund=false\n', { mode: 0o600 });
  await run('npm', policy.install_args);
  const esbuild = policy.optional_policy.esbuild;
  await installOptional(esbuild.packages[platform]);
  const amp = policy.optional_policy.amp_cli;
  const ampPackage = await installOptional(amp.packages[platform]);
  const source = join(ampPackage, process.platform === 'win32' ? amp.source_binary.win32 : amp.source_binary.unix);
  const target = join(staging, amp.target_binary);
  await mkdir(join(staging, 'node_modules', '@ampcode', 'cli', 'bin'), { recursive: true });
  await copyFile(source, target);
  await chmod(target, 0o755);
  for (const asset of policy.preserve_assets) {
    const root = join(staging, 'node_modules', asset.package);
    await stat(root);
    if (asset.asset === 'photon_rs_bg.wasm') await stat(join(root, asset.asset));
  }
  await rm(cache, { recursive: true, force: true });
  await writeFile(join(staging, '.ready'), JSON.stringify({ schema: policy.schema, platform, lock: createHash('sha256').update(await readFile(lockPath)).digest('hex') }), { mode: 0o600 });
  await rename(staging, destination);
  console.error(`Native SDK runtime prepared: ${destination}`);
} catch (error) {
  await rm(staging, { recursive: true, force: true });
  throw error;
}
