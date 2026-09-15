#!/usr/bin/env node
// Shipping declaration inventory for Cockpit presentation states. This is not
// a rendered gallery: Native integration Wave 2 owns compiled state fixtures.
// Screenshots use the CPU reference renderer and never see CoreText.

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

// Each item records where a shipping state belongs. Source declarations prove
// only identity/configuration; interactive rest/hover/press rendering remains
// deliberately unclaimed until compiled native fixtures exist.
export const shippingStateInventory = Object.freeze([
  Object.freeze({ state: 'rest', file: 'windows/components/cockpit-window.native',
    pattern: null, evidence: 'compiled native fixture reserved for integration Wave 2' }),
  Object.freeze({ state: 'hover', file: 'windows/components/cockpit-window.native',
    pattern: null, evidence: 'compiled native fixture reserved for integration Wave 2' }),
  Object.freeze({ state: 'press', file: 'windows/components/cockpit-window.native',
    pattern: null, evidence: 'compiled native fixture reserved for integration Wave 2' }),
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
    pattern: '<template name="cockpit-agents"', evidence: 'shipping inspector stays hit-testable; rendered fill is a Zig contract' }),
]);

const requiredPassiveHoverZigTest = 'semantic_theme test "passive panel hover is visually stable without disabling hit testing"';
export const strictAcceptanceCommand = 'PHUX_COCKPIT_ACCEPTANCE_STRICT=1 node --import ./src/tests/navigation-loader.mjs --test src/tests/presentation-acceptance.test.mjs';

export const evidenceScope = Object.freeze({
  snapshot: 'layout, semantic state, and shipping control identity',
  screenshot: 'CPU reference-renderer geometry, colors, and emitted draw commands only',
  passiveHover: 'runtime hovered state proves pointer reachability only; rest/hover fill equality belongs to the deterministic Zig renderer recipe',
  liveScreenshot: 'optional only with a region-level comparison that tolerates terminal cursor/output changes; never use full-frame PNG equality',
  excluded: 'CoreText outlines, hinting, smoothing, host blending, and display color conversion',
});

export function inspectShippingStateInventory(root) {
  const rootPath = root instanceof URL ? fileURLToPath(root) : root;
  const missing = [];
  for (const item of shippingStateInventory) {
    if (item.pattern === null) continue;
    const source = readFileSync(path.join(rootPath, 'src', item.file), 'utf8');
    if (!source.includes(item.pattern)) missing.push(`${item.state}: ${item.file} lacks ${JSON.stringify(item.pattern)}`);
  }
  return { kind: 'shipping-state-inventory', missing, states: shippingStateInventory.map(item => item.state),
    unrenderedStates: shippingStateInventory.filter(item => item.pattern === null).map(item => item.state),
    sizes: declaredSizes, densities: declaredDensities, evidenceScope,
    renderedStateGallery: 'reserved for compiled native integration Wave 2',
    strictAcceptanceCommand,
    crossCommitZigRequirement: `full Zig gate must include ${requiredPassiveHoverZigTest}` };
}

function stateFlags(line) {
  const match = /\bstate=\[([^\]]*)\]/.exec(line);
  return new Set(match && match[1] ? match[1].split(',') : []);
}

// This proves only that the runtime pointer journey reached the named shipping
// surface. Visual acceptance belongs to requiredPassiveHoverZigTest: SDK panels
// intentionally remain hit-testable and hovered when hover fill equals rest.
export function assertPointerReachedSurface(snapshot, { role = 'group', name }) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const lines = snapshot.split(/\r?\n/).filter(line =>
    new RegExp(`\\brole=${role} name="${escaped}"(?: |$)`).test(line));
  assert.equal(lines.length, 1,
    `pointer probe expected exactly one role=${role} name=${JSON.stringify(name)}, found ${lines.length}`);
  assert.equal(stateFlags(lines[0]).has('hovered'), true,
    `pointer did not reach role=${role} name=${JSON.stringify(name)}`);
}

function usage() {
  console.error('usage: cockpit-state-inventory.mjs [--root <cockpit-root>]');
  console.error('       cockpit-state-inventory.mjs --check-pointer-target <snapshot.txt> --target-name <name> [--target-role <role>]');
}

function main(argv) {
  let root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  let snapshotPath = '';
  let targetName = '';
  let targetRole = 'group';
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--root') root = path.resolve(argv[++index] ?? '');
    else if (arg === '--check-pointer-target') snapshotPath = argv[++index] ?? '';
    else if (arg === '--target-name') targetName = argv[++index] ?? '';
    else if (arg === '--target-role') targetRole = argv[++index] ?? '';
    else if (arg === '-h' || arg === '--help') { usage(); return 0; }
    else { usage(); return 2; }
  }
  if (snapshotPath || targetName) {
    if (!snapshotPath || !targetName || !targetRole) { usage(); return 2; }
    assertPointerReachedSurface(readFileSync(snapshotPath, 'utf8'), { role: targetRole, name: targetName });
    console.log(`PASS pointer reached shipping surface ${JSON.stringify(targetName)}`);
    return 0;
  }
  const report = inspectShippingStateInventory(root);
  if (report.missing.length > 0) {
    console.error(`shipping state inventory is incomplete:\n${report.missing.map(item => `  - ${item}`).join('\n')}`);
    return 1;
  }
  console.log(JSON.stringify(report, null, 2));
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = main(process.argv.slice(2));
  } catch (error) {
    console.error(`shipping state inventory failed: ${error.message}`);
    process.exitCode = 1;
  }
}
