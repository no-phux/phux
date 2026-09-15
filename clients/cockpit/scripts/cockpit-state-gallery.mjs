#!/usr/bin/env node
// Executable inventory for Cockpit's presentation states. This deliberately
// records semantic state and command emission, not glyph fidelity: Native's
// screenshot command uses the CPU reference renderer and never sees CoreText.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

export const declaredSizes = Object.freeze([
  Object.freeze({ width: 900, height: 420 }),
  Object.freeze({ width: 1100, height: 640 }),
  Object.freeze({ width: 1680, height: 1000 }),
]);

export const declaredDensities = Object.freeze(['compact', 'regular', 'spacious']);

// Each state names a shipping declaration that can render it. Keeping this
// next to the capture tool prevents a gallery made from substitute controls
// from passing while the product controls drift.
export const stateGallery = Object.freeze([
  Object.freeze({ state: 'rest', file: 'windows/components/cockpit-window.native',
    pattern: 'label="Inspect agents"', evidence: 'shipping ghost button at rest' }),
  Object.freeze({ state: 'hover', file: 'windows/components/cockpit-window.native',
    pattern: 'label="Inspect agents"', evidence: 'same shipping button under pointer hover' }),
  Object.freeze({ state: 'press', file: 'windows/components/cockpit-window.native',
    pattern: 'label="Inspect agents"', evidence: 'same shipping button while pointer is down' }),
  Object.freeze({ state: 'selected', file: 'windows/components/cockpit-window.native',
    pattern: 'selected="{navigatorView == 1}"', evidence: 'current navigator destination' }),
  Object.freeze({ state: 'focus', file: 'windows/components/cockpit-window.native',
    pattern: 'autofocus="true"', evidence: 'keyboard-owned navigator control' }),
  Object.freeze({ state: 'disabled', file: 'windows/components/cockpit-settings.native',
    pattern: 'disabled="{appearanceBusy}"', evidence: 'pending Settings transaction' }),
  Object.freeze({ state: 'attention', file: 'windows/components/cockpit-window.native',
    pattern: '<if test="{tab.attention}">', evidence: 'provider-backed tab attention' }),
  Object.freeze({ state: 'loading', file: 'windows/components/cockpit-window.native',
    pattern: '{palettenotice}', evidence: 'navigator loading notice from the public model' }),
  Object.freeze({ state: 'empty', file: 'windows/components/cockpit-window.native',
    pattern: '<template name="cockpit-empty"', evidence: 'shipping empty-session surface' }),
  Object.freeze({ state: 'failed', file: 'windows/components/cockpit-window.native',
    pattern: '<if test="{machines.failed}">', evidence: 'shipping machine failure surface' }),
  Object.freeze({ state: 'passive-hover', file: 'windows/components/cockpit-window.native',
    pattern: '<template name="cockpit-agents"', evidence: 'inspector panel must not gain a hover wash' }),
]);

export const evidenceScope = Object.freeze({
  snapshot: 'layout, semantic state, and shipping control identity',
  screenshot: 'CPU reference-renderer geometry, colors, and emitted draw commands only',
  excluded: 'CoreText outlines, hinting, smoothing, host blending, and display color conversion',
});

export function inspectShippingGallery(root) {
  const rootPath = root instanceof URL ? fileURLToPath(root) : root;
  const missing = [];
  for (const item of stateGallery) {
    const source = readFileSync(path.join(rootPath, 'src', item.file), 'utf8');
    if (!source.includes(item.pattern)) missing.push(`${item.state}: ${item.file} lacks ${JSON.stringify(item.pattern)}`);
  }
  return { missing, states: stateGallery.map(item => item.state), sizes: declaredSizes, densities: declaredDensities,
    evidenceScope };
}

function stateFlags(line) {
  const match = /\bstate=\[([^\]]*)\]/.exec(line);
  return new Set(match && match[1] ? match[1].split(',') : []);
}

// Runtime snapshots can be fed here after moving the pointer over a named
// passive surface. A hovered flag is the product failure; screenshots are not
// consulted because this is a semantic interaction assertion, not pixel work.
export function assertPassiveSurface(snapshot, { role = 'group', name }) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const lines = snapshot.split(/\r?\n/).filter(line =>
    new RegExp(`\\brole=${role} name="${escaped}"(?: |$)`).test(line));
  assert.equal(lines.length, 1,
    `passive-surface probe expected exactly one role=${role} name=${JSON.stringify(name)}, found ${lines.length}`);
  assert.equal(stateFlags(lines[0]).has('hovered'), false,
    `passive surface ${JSON.stringify(name)} hover-highlighted; use a non-interactive surface recipe or suppress its hover wash`);
}

function usage() {
  console.error('usage: cockpit-state-gallery.mjs [--root <cockpit-root>]');
  console.error('       cockpit-state-gallery.mjs --check-snapshot <snapshot.txt> --passive-name <name> [--passive-role <role>]');
}

function main(argv) {
  let root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  let snapshotPath = '';
  let passiveName = '';
  let passiveRole = 'group';
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--root') root = path.resolve(argv[++index] ?? '');
    else if (arg === '--check-snapshot') snapshotPath = argv[++index] ?? '';
    else if (arg === '--passive-name') passiveName = argv[++index] ?? '';
    else if (arg === '--passive-role') passiveRole = argv[++index] ?? '';
    else if (arg === '-h' || arg === '--help') { usage(); return 0; }
    else { usage(); return 2; }
  }
  if (snapshotPath || passiveName) {
    if (!snapshotPath || !passiveName || !passiveRole) { usage(); return 2; }
    assertPassiveSurface(readFileSync(snapshotPath, 'utf8'), { role: passiveRole, name: passiveName });
    console.log(`PASS passive surface ${JSON.stringify(passiveName)} has no hover state`);
    return 0;
  }
  const report = inspectShippingGallery(root);
  if (report.missing.length > 0) {
    console.error(`state gallery is incomplete:\n${report.missing.map(item => `  - ${item}`).join('\n')}`);
    return 1;
  }
  console.log(JSON.stringify(report, null, 2));
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = main(process.argv.slice(2));
  } catch (error) {
    console.error(`state gallery failed: ${error.message}`);
    process.exitCode = 1;
  }
}
