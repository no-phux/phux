// Regenerates only the Apache-2.0 app-server protocol, never CLI/commercial runtime code.
// Preconditions: Node >=22.19, an explicit existing Codex executable matching source.json,
// and a clean task-owned generated/ + schema/ subtree. No npm install or formatter is invoked.
// From repository root:
// node integrations/native-host/src/codex-protocol/generate.mjs /absolute/path/to/codex
import { execFile } from 'node:child_process';
import { mkdtemp, readFile, writeFile, mkdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, posix } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execute = promisify(execFile);
const directory = dirname(fileURLToPath(import.meta.url));
const pin = JSON.parse(await readFile(join(directory, 'source.json'), 'utf8'));
const [executable, ...extra] = process.argv.slice(2);
if (!executable || !isAbsolute(executable) || extra.length) {
  throw new Error('Supply exactly one absolute existing Codex executable path');
}
const scratch = await mkdtemp(join(tmpdir(), 'phux-codex-protocol-'));
try {
  const env = { ...process.env, CODEX_HOME: join(scratch, 'home') };
  const version = await execute(executable, ['--version'], { env, timeout: 10000, maxBuffer: 4096 });
  if (version.stdout.trim() !== `codex-cli ${pin.version}`) throw new Error(`Expected Codex ${pin.version}`);
  const types = join(scratch, 'typescript');
  const schemas = join(scratch, 'json');
  // Stable API exports match the pinned official schema fixtures. No --prettier argument.
  await execute(executable, ['app-server', 'generate-ts', '--out', types], { env, timeout: 30000 });
  await execute(executable, ['app-server', 'generate-json-schema', '--out', schemas], { env, timeout: 30000 });
  const selected = new Map();
  const pending = [...pin.typescriptRoots];
  while (pending.length) {
    const name = pending.pop();
    if (selected.has(name)) continue;
    if (name.startsWith('../') || posix.isAbsolute(name)) throw new Error('Invalid generated import path');
    const text = await readFile(join(types, name), 'utf8');
    selected.set(name, text);
    for (const match of text.matchAll(/from\s+["']([^"']+)["']/g)) {
      const dependency = posix.normalize(posix.join(posix.dirname(name), match[1]));
      pending.push(dependency.replace(/\.(?:js|ts)$/, '') + '.ts');
    }
  }
  // Read every output before modifying the task-owned vendored subtree.
  const schemaFiles = new Map(await Promise.all(pin.schemas.map(async (name) => [name, await readFile(join(schemas, name), 'utf8')])));
  for (const [name, text] of selected) {
    const target = join(directory, 'generated', name);
    await mkdir(dirname(target), { recursive: true });
    await writeFile(target, text);
  }
  for (const [name, text] of schemaFiles) {
    const target = join(directory, 'schema', name);
    await mkdir(dirname(target), { recursive: true });
    await writeFile(target, text);
  }
  for (const name of pin.files) {
    if (!selected.has(name)) await rm(join(directory, 'generated', name));
  }
  pin.files = [...selected.keys()].sort();
  await writeFile(join(directory, 'source.json'), JSON.stringify(pin, null, 2) + '\n');
} finally {
  await rm(scratch, { recursive: true, force: true });
}
