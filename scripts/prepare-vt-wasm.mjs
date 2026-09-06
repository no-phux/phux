// Remove non-semantic debug metadata and verify the Rust consumer's frozen ABI
// before publishing a rebuilt engine. No optimizer or extra tool version pin.
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { webcrypto } from 'node:crypto';

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
let memory;
const { instance } = await WebAssembly.instantiate(stripped, {
  env: { log() {} },
  ghostty: {
    host_entropy_fill(ptr, len) {
      try {
        for (let i = 0; i < len; i += 65536) {
          webcrypto.getRandomValues(new Uint8Array(memory.buffer, ptr + i, Math.min(65536, len - i)));
        }
        return 0;
      } catch { return -1; }
    },
  },
});
const e = instance.exports;
memory = e.memory;
const jsonPtr = e.ghostty_type_json();
const jsonBytes = new Uint8Array(memory.buffer);
const layouts = JSON.parse(new TextDecoder().decode(jsonBytes.subarray(jsonPtr, jsonBytes.indexOf(0, jsonPtr))));
// Decoder buffers are allocated by Rust at fixed sizes and read at these
// offsets. Comparing against constants catches a self-consistent upstream ABI
// change that its own smoke (which discovers layouts dynamically) would accept.
for (const [name, size, fields] of [
  ['DecoderOptions', 20, { size: 0, version: 4, max_continuation_bytes: 8, max_record_bytes: 12, max_pages: 16 }],
  ['DecodeEvent', 36, { size: 0, version: 4, kind: 8, codec_version: 12, screen_key: 14, index: 16, count: 20, retained: 24, consumed: 28, needed: 32 }],
  ['TakeTerminalResult', 16, { size: 0, version: 4, terminal: 8, codec_version: 12 }],
]) {
  const layout = layouts[`GhosttyTerminalSnapshot${name}`];
  assert.equal(layout?.size, size, `${name} size`);
  for (const [field, offset] of Object.entries(fields)) {
    assert.equal(layout.fields[field]?.offset, offset, `${name}.${field}`);
  }
}
const ptr = e.ghostty_alloc(0, 56);
assert.ok(ptr, 'capability allocation');
new Uint8Array(memory.buffer, ptr, 56).fill(0);
new DataView(memory.buffer).setUint32(ptr, 56, true);
assert.equal(e.ghostty_terminal_snapshot_incremental_capabilities(ptr), 0);
const view = new DataView(memory.buffer);
// These offsets intentionally match phux-vt-web's probe, rather than querying
// the candidate to tell us what layout it wants us to accept.
assert.equal(view.getUint32(ptr + 4, true), 1, 'incremental ABI version');
assert.ok(view.getUint16(ptr + 8, true) <= 2, 'minimum decode version');
assert.ok(view.getUint16(ptr + 10, true) >= 2, 'maximum decode version');
assert.equal(view.getUint16(ptr + 12, true), 2, 'encode version');
for (let i = 14; i <= 20; i++) assert.equal(view.getUint8(ptr + i), 1, `capability at ${i}`);
assert.ok(view.getUint32(ptr + 24, true) >= 1024 * 1024, 'record limit');
for (const i of [28, 32, 36]) assert.ok(view.getUint32(ptr + i, true) > 0, `limit at ${i}`);
function stringAt(offset) {
  const start = view.getUint32(ptr + offset, true);
  const length = view.getUint32(ptr + offset + 4, true);
  return new TextDecoder().decode(new Uint8Array(memory.buffer, start, length));
}
assert.equal(stringAt(40), 'ghostty.snapshot.v1-v2.incremental.v1', 'codec identity');
assert.ok(stringAt(48).length > 0, 'build identity');
e.ghostty_free(0, ptr, 56);
writeFileSync(path, stripped);
console.log(`frozen consumer ABI verified; stripped ${bytes.length - stripped.length} metadata bytes`);
