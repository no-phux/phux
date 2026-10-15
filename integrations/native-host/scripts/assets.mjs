import { createHash } from 'node:crypto';
import { mkdir, open, rm } from 'node:fs/promises';
import { join } from 'node:path';

// Callers select only the reviewed host package from runtime-policy.json.
// Registry provenance and SHA-512 are checked before any archive is decoded.
export async function installPinnedAsset(name, { lock, policy, root, run }) {
  if (!name || policy.optional_policy.never_install_prefixes.some(prefix => name.startsWith(prefix))) throw new Error('Unapproved optional SDK package');
  const entry = lock.packages[`node_modules/${name}`];
  if (!entry?.resolved || !entry.integrity?.startsWith('sha512-')) throw new Error(`SDK lock lacks SHA-512 integrity for ${name}`);
  const url = new URL(entry.resolved);
  if (url.protocol !== 'https:' || url.hostname !== 'registry.npmjs.org') throw new Error('SDK asset must use the locked npm registry URL');
  const response = await fetch(url, { redirect: 'error' });
  if (!response.ok || !response.body) throw new Error(`SDK asset HTTP ${response.status}`);
  const archive = join(root, `${name.replaceAll('/', '-')}.tgz`);
  const file = await open(archive, 'wx', 0o600);
  const hash = createHash('sha512');
  let total = 0;
  try {
    for await (const chunk of response.body) {
      total += chunk.byteLength;
      if (total > 128 * 1024 * 1024) throw new Error('SDK asset exceeds 128MiB');
      hash.update(chunk);
      let offset = 0;
      while (offset < chunk.byteLength) {
        const { bytesWritten } = await file.write(chunk, offset, chunk.byteLength - offset);
        if (bytesWritten === 0) throw new Error('SDK asset write made no progress');
        offset += bytesWritten;
      }
    }
  } finally { await file.close(); }
  if (`sha512-${hash.digest('base64')}` !== entry.integrity) throw new Error(`SDK asset integrity mismatch for ${name}`);
  const target = join(root, 'node_modules', name);
  await mkdir(target, { recursive: true });
  await run('tar', ['-xzf', archive, '--strip-components=1', '-C', target]);
  await rm(archive);
  return target;
}
