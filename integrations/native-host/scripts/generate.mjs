// Own-code bridge only. Packages remain external in the reviewed isolated tree;
// Pi WASM, QuickJS and esbuild assets must not be replaced with bundled JS.
import { build } from 'esbuild';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
await build({
  absWorkingDir: root,
  entryPoints: ['src/index.ts'],
  outfile: 'index.js',
  bundle: true,
  packages: 'external',
  platform: 'node',
  target: 'node22.19',
  format: 'esm',
  sourcemap: false,
  legalComments: 'inline',
  banner: { js: '// Generated from phux own-code TypeScript by npm run generate. No provider SDK is bundled.' },
});
