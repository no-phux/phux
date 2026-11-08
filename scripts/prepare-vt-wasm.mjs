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
// The bell and snapshot-reader callbacks are installed by growing this table.
assert.ok(e.__indirect_function_table instanceof WebAssembly.Table, 'exported function table');
for (const name of [
  'ghostty_wasm_alloc', 'ghostty_wasm_free', 'ghostty_free', 'ghostty_type_json',
  'ghostty_terminal_new', 'ghostty_terminal_free', 'ghostty_terminal_get', 'ghostty_terminal_set',
  'ghostty_terminal_vt_write', 'ghostty_terminal_resize', 'ghostty_terminal_scroll_viewport',
  'ghostty_terminal_grid_ref', 'ghostty_grid_ref_hyperlink_uri',
  'ghostty_formatter_terminal_new', 'ghostty_formatter_set', 'ghostty_formatter_format_alloc', 'ghostty_formatter_free',
  'ghostty_render_state_new', 'ghostty_render_state_free', 'ghostty_render_state_update',
  'ghostty_render_state_get', 'ghostty_render_state_row_iterator_new',
  'ghostty_render_state_row_iterator_free', 'ghostty_render_state_row_iterator_next',
  'ghostty_render_state_row_get', 'ghostty_render_state_row_cells_new',
  'ghostty_render_state_row_cells_free', 'ghostty_render_state_row_cells_next',
  'ghostty_render_state_row_cells_get', 'ghostty_unicode_codepoint_width',
  'ghostty_snapshot_encode_alloc', 'ghostty_snapshot_decoder_new', 'ghostty_snapshot_decoder_free',
  'ghostty_snapshot_decoder_set', 'ghostty_snapshot_decoder_get', 'ghostty_snapshot_decoder_ready',
  'ghostty_snapshot_decoder_next', 'ghostty_snapshot_decoder_next_staged',
  'ghostty_snapshot_validate_records',
]) assert.equal(typeof e[name], 'function', `export ${name}`);
assert.equal(e.ghostty_formatter_terminal_new.length, 3, '1.0 formatter constructor');
assert.equal(e.ghostty_snapshot_decoder_next_staged.length, 2, 'bounded staged decoder');
assert.equal(e.ghostty_snapshot_validate_records.length, 2, 'preapply record CRC validation');
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
  ['GhosttyCodepointBuffer', 12, { ptr: 0, cap: 4, len: 8 }],
  ['GhosttyColorRgb', 3, { r: 0, g: 1, b: 2 }],
  ['GhosttyStyleColor', 4, { tag: 0, value: 1 }],
  ['GhosttyStyle', 16, { fg_color: 0, bg_color: 4, underline_color: 8, flags: 12, reserved: 14 }],
  ['GhosttyTerminalModeConfig', 4, { mode: 0, value: 2 }],
  ['GhosttyTerminalScrollbar', 24, { total: 0, offset: 8, len: 16 }],
  ['GhosttyTerminalScrollViewport', 24, { tag: 0, value: 8 }],
  ['GhosttyPoint', 24, { tag: 0, value: 8 }],
  ['GhosttyPointCoordinate', 8, { x: 0, y: 4 }],
  ['GhosttyGridRef', 8, { node: 0, x: 4, y: 6 }],
  ['GhosttySelection', 20, { start: 0, end: 8, rectangle: 16 }],
  ['GhosttyReader', 8, { read: 0, userdata: 4 }],
]) {
  const layout = types[name];
  assert.equal(layout?.size, size, `${name} size`);
  for (const [field, at] of Object.entries(fields)) {
    assert.equal(layout.fields[field]?.offset, at, `${name}.${field}`);
  }
}
for (const [name, values] of [
  ['GhosttyResult', { SUCCESS: 0, OUT_OF_SPACE: -3, NO_VALUE: -4 }],
  ['GhosttySnapshotDecoderOption', { MAX_CONTINUATION_BYTES: 1 }],
  ['GhosttySnapshotDecoderData', { PROGRESS_ROWS: 6, PROGRESS_REMAINING: 7, PROGRESS_KIND: 11 }],
  ['GhosttySnapshotDecoderProgressKind', { PAGE: 0, IMAGE: 1 }],
  ['GhosttyTerminalOption', { BELL: 3, SCROLLBACK_MAX_BYTES: 28, SCROLLBACK_MAX_LINES: 29 }],
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
  ['GhosttyFormatterOption', { UNWRAP: 2, SELECTION: 4 }],
]) {
  for (const [key, value] of Object.entries(values)) {
    assert.equal(types[name]?.values?.[key], value, `${name}.${key}`);
  }
}
assert.equal(types.GhosttyStyleFlags?.bits?.inverse?.lsb, 4, 'inverse flag bit');
assert.equal(types.GhosttyStyleFlags?.bits?.inverse?.width, 1, 'inverse flag width');
writeFileSync(path, stripped);
console.log(`frozen consumer ABI verified; stripped ${bytes.length - stripped.length} metadata bytes`);
