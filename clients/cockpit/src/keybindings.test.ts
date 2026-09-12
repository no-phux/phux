import { test } from "node:test";
import assert from "node:assert/strict";
import { initialKeybindings, keybindingRequest, keybindingResponse, keybindingHint } from "./keybindings.ts";

const text = (value: string): Uint8Array => new TextEncoder().encode(value);
const string = (value: Uint8Array): string => new TextDecoder().decode(value);

function reply(binding: string, overridden: boolean, notice = ""): Uint8Array {
  const command = text("terminal.new");
  const label = text("New Tab");
  const current = text(binding);
  const defaultBinding = text("Cmd+t");
  return new Uint8Array([1, 1, notice.length > 0 ? 1 : 0, notice.length, ...text(notice),
    0, overridden ? 1 : 0, command.length, label.length, current.length, defaultBinding.length,
    ...command, ...label, ...current, ...defaultBinding]);
}

test("request preserves chord bytes and distinguishes remap, unbind and reset", () => {
  assert.deepEqual(keybindingRequest(1, 2, text("Cmd+Shift+p")), new Uint8Array([1, 1, 2, 11, ...text("Cmd+Shift+p")]));
  assert.deepEqual(keybindingRequest(1, 2, text("none")), new Uint8Array([1, 1, 2, 4, ...text("none")]));
  assert.deepEqual(keybindingRequest(2, 2, text("")), new Uint8Array([1, 2, 2, 0]));
  assert.equal(keybindingRequest(2, 2, text("ignored")).length, 0);
  assert.equal(keybindingRequest(1, 192, text("cmd+t")).length, 0);
  assert.equal(keybindingRequest(4, 0, text("")).length, 0);
  assert.equal(keybindingRequest(1, NaN, text("")).length, 0);
  assert.equal(keybindingRequest(1, 0, new Uint8Array(65)).length, 0);
});

test("menu and palette hints follow installed values, including explicit unbind", () => {
  const defaults = keybindingResponse(reply("Cmd+t", false));
  const remapped = keybindingResponse(reply("Cmd+Shift+t", true));
  const unbound = keybindingResponse(reply("", true));
  assert.ok(defaults && remapped && unbound);
  assert.equal(string(keybindingHint(defaults, text("terminal.new"))), "Cmd+t");
  assert.equal(string(keybindingHint(remapped, text("terminal.new"))), "Cmd+Shift+t");
  assert.equal(keybindingHint(unbound, text("terminal.new")).length, 0);
  assert.equal(string(unbound.rows[0].defaultBinding), "Cmd+t");
  assert.equal(unbound.rows[0].overridden, true);
  assert.equal(keybindingHint(initialKeybindings(), text("terminal.new")).length, 0);
});

test("rejected edits retain accepted hints and expose native failure notice", () => {
  const page = keybindingResponse(reply("Cmd+t", false, "This chord is already assigned."));
  assert.ok(page);
  assert.equal(page.rejected, true);
  assert.equal(string(page.notice), "This chord is already assigned.");
  assert.equal(string(page.rows[0].binding), "Cmd+t");
});

test("decoder rejects truncated rows, bad flags, oversized fields and extra bytes", () => {
  const valid = reply("Cmd+t", false);
  for (let length = 0; length < valid.length; length += 1) assert.equal(keybindingResponse(valid.slice(0, length)), null);
  for (const [offset, value] of [[0, 2], [1, 193], [2, 2], [4, 1], [5, 2], [6, 129], [7, 0], [8, 65], [9, 65]]) {
    const bytes = valid.slice();
    bytes[offset] = value;
    assert.equal(keybindingResponse(bytes), null);
  }
  assert.equal(keybindingResponse(new Uint8Array([...valid, 0])), null);
});
