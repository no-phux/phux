// All labels and hints are received from the installed native registry. The
// AOT coordinator deliberately does not parse chords or duplicate app.zon.
export interface KeybindingRow {
  readonly index: number;
  readonly command: Uint8Array;
  readonly label: Uint8Array;
  readonly binding: Uint8Array;
  readonly defaultBinding: Uint8Array;
  readonly overridden: boolean;
}

export interface KeybindingPage {
  readonly rows: readonly KeybindingRow[];
  readonly notice: Uint8Array;
  readonly rejected: boolean;
}

export function initialKeybindings(): KeybindingPage {
  return { rows: [], notice: new Uint8Array(0), rejected: false };
}

// Actions: 0 describe, 1 remap ("none" unbinds), 2 reset row, 3 reset all.
export function keybindingRequest(action: number, index: number, value: Uint8Array): Uint8Array {
  if (!validRequest(action, index, value.length)) return new Uint8Array(0);
  const bytes = new Uint8Array(4 + value.length);
  bytes[0] = 1;
  bytes[1] = action;
  bytes[2] = index;
  bytes[3] = value.length;
  for (let offset = 0; offset < value.length; offset += 1) bytes[4 + offset] = value[offset];
  return bytes;
}

function validRequest(action: number, index: number, length: number): boolean {
  if (!boundedInteger(action, 3)) return false;
  if (!boundedInteger(index, 191)) return false;
  if (length > 64) return false;
  return action === 1 || length === 0;
}

function boundedInteger(value: number, maximum: number): boolean {
  return value >= 0 && value <= maximum && value === Math.trunc(value);
}

function validHeader(bytes: Uint8Array): boolean {
  if (bytes.length < 4 || bytes[0] !== 1) return false;
  if (bytes[1] > 192 || bytes[2] > 1) return false;
  return 4 + bytes[3] <= bytes.length;
}

function validRow(bytes: Uint8Array, offset: number, index: number): boolean {
  if (offset + 6 > bytes.length) return false;
  if (bytes[offset] !== index || bytes[offset + 1] > 1) return false;
  return validRowLengths(bytes, offset);
}

function validRowLengths(bytes: Uint8Array, offset: number): boolean {
  if (bytes[offset + 2] === 0 || bytes[offset + 2] > 128) return false;
  if (bytes[offset + 3] === 0 || bytes[offset + 3] > 128) return false;
  return bytes[offset + 4] <= 64 && bytes[offset + 5] <= 64;
}

export function keybindingResponse(bytes: Uint8Array): KeybindingPage | null {
  if (!validHeader(bytes)) return null;
  const rows: KeybindingRow[] = [];
  let offset = 4 + bytes[3];
  for (let index = 0; index < bytes[1]; index += 1) {
    if (!validRow(bytes, offset, index)) return null;
    const commandStart = offset + 6;
    const labelStart = commandStart + bytes[offset + 2];
    const bindingStart = labelStart + bytes[offset + 3];
    const defaultStart = bindingStart + bytes[offset + 4];
    const end = defaultStart + bytes[offset + 5];
    if (end > bytes.length) return null;
    // scriptc needs the whole-number proof at the record construction site;
    // validHeader's byte/count guard is not propagated across helper calls.
    const rowIndex = index >= 0 && index <= 191 ? Math.trunc(index) : 0;
    rows.push({ index: rowIndex, command: bytes.slice(commandStart, labelStart), label: bytes.slice(labelStart, bindingStart),
      binding: bytes.slice(bindingStart, defaultStart), defaultBinding: bytes.slice(defaultStart, end),
      overridden: bytes[offset + 1] === 1 });
    offset = end;
  }
  if (offset !== bytes.length) return null;
  return { rows, notice: bytes.slice(4, 4 + bytes[3]), rejected: bytes[2] === 1 };
}

function sameCommand(left: Uint8Array, right: Uint8Array): boolean {
  if (left.length !== right.length) return false;
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}

// An empty result means unbound or not yet discovered; never fall back to a
// static hint after a user has removed/remapped that chord.
export function keybindingHint(page: KeybindingPage, command: Uint8Array): Uint8Array {
  for (const row of page.rows) {
    if (sameCommand(row.command, command)) return row.binding;
  }
  return new Uint8Array(0);
}
