// Remove non-semantic debug metadata and verify the Rust consumer's frozen ABI
// before publishing a rebuilt engine. No optimizer or extra tool version pin.
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';

const path = process.argv[2];
assert.ok(path, 'usage: node scripts/prepare-vt-wasm.mjs ARTIFACT');
const bytes = readFileSync(path);
await WebAssembly.compile(bytes); // Validate section bounds/LEB encodings first.
let offset = 8;
function uleb() {
  let value = 0, shift = 0, byte;
  do {
    byte = bytes[offset++];
    value += (byte & 127) * 2 ** shift;
    shift += 7;
  } while (byte & 128);
  return value;
}
const sections = [bytes.subarray(0, 8)];
while (offset < bytes.length) {
  const start = offset;
  const id = bytes[offset++];
  const length = uleb();
  const end = offset + length;
  // Custom sections contain debug paths, names and producer metadata; none
  // affect execution. Preserve every standard section byte-for-byte.
  if (id !== 0) sections.push(bytes.subarray(start, end));
  offset = end;
}
const stripped = Buffer.concat(sections);
const module = await WebAssembly.compile(stripped);
// phux-vt-web instantiates the engine with an empty import object.
assert.deepEqual(WebAssembly.Module.imports(module), [], 'engine imports nothing');
const { exports: e } = await WebAssembly.instantiate(module, {});
assert.ok(e.memory instanceof WebAssembly.Memory, 'exported memory');
// The bell callback is installed by growing this table.
assert.ok(e.__indirect_function_table instanceof WebAssembly.Table, 'exported function table');
for (const name of [
  'ghostty_wasm_alloc', 'ghostty_wasm_free', 'ghostty_free', 'ghostty_type_json',
  'ghostty_terminal_new', 'ghostty_terminal_free', 'ghostty_terminal_get', 'ghostty_terminal_set',
  'ghostty_terminal_vt_write', 'ghostty_terminal_resize', 'ghostty_terminal_scroll_viewport',
  'ghostty_terminal_grid_ref', 'ghostty_grid_ref_hyperlink_uri', 'ghostty_terminal_selection_format_alloc',
  'ghostty_formatter_terminal_new', 'ghostty_formatter_format_alloc', 'ghostty_formatter_free',
  'ghostty_render_state_new', 'ghostty_render_state_free', 'ghostty_render_state_update',
  'ghostty_render_state_get', 'ghostty_render_state_row_iterator_new',
  'ghostty_render_state_row_iterator_free', 'ghostty_render_state_row_iterator_next',
  'ghostty_render_state_row_get', 'ghostty_render_state_row_cells_new',
  'ghostty_render_state_row_cells_free', 'ghostty_render_state_row_cells_next',
  'ghostty_render_state_row_cells_get', 'ghostty_unicode_codepoint_width',
]) assert.equal(typeof e[name], 'function', `export ${name}`);
const jsonPtr = e.ghostty_type_json();
const jsonBytes = new Uint8Array(e.memory.buffer);
const { abi, types } = JSON.parse(new TextDecoder().decode(jsonBytes.subarray(jsonPtr, jsonBytes.indexOf(0, jsonPtr))));
assert.equal(abi?.pointer_size, 4, 'wasm32 pointers');
assert.equal(abi.usize_size, 4, 'wasm32 size_t');
assert.equal(abi.endian, 'little', 'little-endian');
// phux-vt-web reads and writes these structs at fixed sizes and offsets, and
// passes these enum tags as literals. Comparing against constants that match
// clients/phux-vt-web/src/lib.rs, rather than trusting the candidate's own
// manifest, catches a self-consistent upstream ABI change.
for (const [name, size, fields] of [
  ['GhosttyString', 8, { ptr: 0, len: 4 }],
  ['GhosttyColorRgb', 3, { r: 0, g: 1, b: 2 }],
  ['GhosttyStyle', 72, { size: 0, inverse: 60 }],
  ['GhosttyTerminalModeConfig', 4, { mode: 0, value: 2 }],
  ['GhosttyTerminalScrollbar', 24, { total: 0, offset: 8, len: 16 }],
  ['GhosttyTerminalScrollViewport', 24, { tag: 0, value: 8 }],
  ['GhosttyPoint', 24, { tag: 0, value: 8 }],
  ['GhosttyPointCoordinate', 8, { x: 0, y: 4 }],
  ['GhosttyGridRef', 12, { size: 0, node: 4, x: 8, y: 10 }],
  ['GhosttySelection', 32, { size: 0, start: 4, end: 16, rectangle: 28 }],
  ['GhosttyTerminalSelectionFormatOptions', 16, { size: 0, emit: 4, unwrap: 8, trim: 9, selection: 12 }],
  ['GhosttyFormatterTerminalOptions', 40, { size: 0, emit: 4, unwrap: 8, trim: 9, extra: 12, selection: 36 }],
  ['GhosttyFormatterTerminalExtra', 24, { size: 0, screen: 12 }],
  ['GhosttyFormatterScreenExtra', 12, { size: 0 }],
]) {
  const layout = types[name];
  assert.equal(layout?.size, size, `${name} size`);
  for (const [field, at] of Object.entries(fields)) {
    assert.equal(layout.fields[field]?.offset, at, `${name}.${field}`);
  }
}
for (const [name, values] of [
  ['GhosttyResult', { SUCCESS: 0, OUT_OF_SPACE: -3 }],
  ['GhosttyTerminalOption', { BELL: 2, SCROLLBACK_MAX_BYTES: 27, SCROLLBACK_MAX_LINES: 28 }],
  ['GhosttyTerminalData', {
    CURSOR_X: 3, CURSOR_Y: 4, CURSOR_VISIBLE: 7, SCROLLBAR: 9, MOUSE_TRACKING: 11, TITLE: 12,
    VIEWPORT_ACTIVE: 32, SCROLLBACK_MAX_BYTES: 34, SCROLLBACK_MAX_LINES: 35, MODE: 37,
  }],
  ['GhosttyRenderStateData', { COLS: 1, ROWS: 2, ROW_ITERATOR: 4, COLOR_BACKGROUND: 5, COLOR_FOREGROUND: 6 }],
  ['GhosttyRenderStateRowData', { CELLS: 3 }],
  ['GhosttyRenderStateRowCellsData', { STYLE: 2, GRAPHEMES_LEN: 3, GRAPHEMES_BUF: 4, BG_COLOR: 5, FG_COLOR: 6 }],
  ['GhosttyPointTag', { VIEWPORT: 1 }],
  ['GhosttyTerminalScrollViewportTag', { TOP: 0, BOTTOM: 1, DELTA: 2 }],
  ['GhosttyFormatterFormat', { PLAIN: 0 }],
]) {
  for (const [key, value] of Object.entries(values)) {
    assert.equal(types[name]?.values?.[key], value, `${name}.${key}`);
  }
}
writeFileSync(path, stripped);
console.log(`frozen consumer ABI verified; stripped ${bytes.length - stripped.length} metadata bytes`);
