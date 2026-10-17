// Behavioral smoke of a ghostty-vt.wasm candidate through the calls
// phux-vt-web makes: write VT bytes, then read the cursor, title, a DEC mode,
// and a styled cell back. Run after prepare-vt-wasm.mjs has frozen the ABI.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const path = process.argv[2];
assert.ok(path, 'usage: node scripts/smoke-vt-wasm.mjs ARTIFACT');
const { instance } = await WebAssembly.instantiate(readFileSync(path), {});
const e = instance.exports;
const view = () => new DataView(e.memory.buffer);
const ok = (status, what) => assert.equal(status, 0, `${what} failed with ${status}`);
const slot = e.ghostty_wasm_alloc_opaque();
const opaque = (status, what) => {
  ok(status, what);
  const handle = e.ghostty_wasm_take_opaque(slot);
  assert.notEqual(handle, 0, `${what} handle`);
  return handle;
};
const terminal = opaque(e.ghostty_terminal_new(0, slot, 20, 3), 'terminal_new');
const input = new TextEncoder().encode('\x1b]2;smoke ✓\x07\x1b[?1002h\x1b[38;2;255;0;0mR\x1b[0m');
const inputPtr = e.ghostty_wasm_alloc(input.length);
new Uint8Array(e.memory.buffer, inputPtr, input.length).set(input);
e.ghostty_terminal_vt_write(terminal, inputPtr, input.length);
e.ghostty_wasm_free(inputPtr, input.length);

const out = e.ghostty_wasm_alloc(8);
const get = (tag, what) => {
  view().setBigUint64(out, 0n, true);
  ok(e.ghostty_terminal_get(terminal, tag, out), what);
};
get(3, 'CURSOR_X');
assert.equal(view().getUint16(out, true), 1, 'cursor advanced past R');
get(12, 'TITLE');
const titlePtr = view().getUint32(out, true);
const title = new Uint8Array(e.memory.buffer, titlePtr, view().getUint32(out + 4, true));
assert.equal(new TextDecoder().decode(title), 'smoke ✓', 'OSC 2 title');
for (const [mode, expected] of [[1002, true], [1003, false]]) {
  view().setUint32(out, mode, true); // GhosttyTerminalModeConfig { mode, value: false }
  ok(e.ghostty_terminal_get(terminal, 37, out), `MODE ${mode}`);
  assert.equal(view().getUint8(out + 2) !== 0, expected, `DEC mode ${mode}`);
}

const state = opaque(e.ghostty_render_state_new(0, slot), 'render_state_new');
const rows = opaque(e.ghostty_render_state_row_iterator_new(0, slot), 'row_iterator_new');
const cells = opaque(e.ghostty_render_state_row_cells_new(0, slot), 'row_cells_new');
ok(e.ghostty_render_state_update(state, terminal), 'render_state_update');
ok(e.ghostty_render_state_get(state, 1, out), 'render COLS');
assert.equal(view().getUint16(out, true), 20, 'render cols');
view().setUint32(out, rows, true);
ok(e.ghostty_render_state_get(state, 4, out), 'render ROW_ITERATOR');
assert.notEqual(e.ghostty_render_state_row_iterator_next(rows), 0, 'first row');
view().setUint32(out, cells, true);
ok(e.ghostty_render_state_row_get(rows, 3, out), 'row CELLS');
assert.notEqual(e.ghostty_render_state_row_cells_next(cells), 0, 'first cell');
ok(e.ghostty_render_state_row_cells_get(cells, 4, out), 'cell GRAPHEMES_BUF');
assert.equal(view().getUint32(out, true), 'R'.codePointAt(0), 'first cell grapheme');
ok(e.ghostty_render_state_row_cells_get(cells, 6, out), 'cell FG_COLOR');
assert.deepEqual([0, 1, 2].map((i) => view().getUint8(out + i)), [255, 0, 0], 'truecolor fg');

e.ghostty_wasm_free(out, 8);
e.ghostty_render_state_row_cells_free(cells);
e.ghostty_render_state_row_iterator_free(rows);
e.ghostty_render_state_free(state);
e.ghostty_terminal_free(terminal);
e.ghostty_wasm_free_opaque(slot);
console.log(`engine smoke passed: ${path}`);
