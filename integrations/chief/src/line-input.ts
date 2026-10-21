/**
 * A raw-mode line assembler. Canonical mode would cap a line at the
 * kernel's MAX_CANON (1024 bytes on macOS), which a JSON envelope from the
 * phone easily exceeds, so the chief reads raw bytes and assembles lines
 * itself: printable text and bracketed pastes (newlines kept) fill the
 * buffer, Enter submits it, Backspace edits it, Ctrl-C interrupts.
 */

export type InputEvent =
  | { readonly type: "submit"; readonly text: string }
  | { readonly type: "interrupt" }
  | { readonly type: "echo"; readonly text: string };

const PASTE_START = "\u001b[200~";
const PASTE_END = "\u001b[201~";
/** Bracketed paste on: the terminal wraps pasted text in PASTE_START/PASTE_END. */
export const ENABLE_BRACKETED_PASTE = "\u001b[?2004h";
export const DISABLE_BRACKETED_PASTE = "\u001b[?2004l";
const MAX_BUFFER = 1024 * 1024;

export class LineInput {
  private buffer = "";
  private pasting = false;
  private pending = "";

  feed(chunk: string): InputEvent[] {
    const events: InputEvent[] = [];
    let data = this.pending + chunk;
    this.pending = "";
    while (data.length > 0) {
      if (this.pasting) {
        const end = data.indexOf(PASTE_END);
        if (end < 0) {
          const keep = partialSuffix(data, PASTE_END);
          this.append(data.slice(0, data.length - keep), events);
          this.pending = data.slice(data.length - keep);
          return events;
        }
        this.append(data.slice(0, end), events);
        this.pasting = false;
        data = data.slice(end + PASTE_END.length);
        continue;
      }
      const char = data[0]!;
      if (char === "\u001b") {
        if (data.startsWith(PASTE_START)) {
          this.pasting = true;
          data = data.slice(PASTE_START.length);
          continue;
        }
        const length = escapeLength(data);
        if (length === 0) {
          this.pending = data;
          return events;
        }
        data = data.slice(length);
        continue;
      }
      data = data.slice(1);
      if (char === "\r" || char === "\n") {
        // A CRLF pair is one Enter.
        if (char === "\r" && data[0] === "\n") data = data.slice(1);
        const text = this.buffer;
        this.buffer = "";
        events.push({ type: "echo", text: "\r\n" });
        if (text.trim().length > 0) events.push({ type: "submit", text });
      } else if (char === "\u0003") {
        this.buffer = "";
        events.push({ type: "echo", text: "^C\r\n" }, { type: "interrupt" });
      } else if (char === "\u007f" || char === "\b") {
        if (this.buffer.length > 0) {
          const chars = Array.from(this.buffer);
          const removed = chars.pop()!;
          this.buffer = chars.join("");
          if (removed !== "\n") events.push({ type: "echo", text: "\b \b" });
        }
      } else if (char === "\u0015") {
        // Ctrl-U clears the line.
        this.buffer = "";
        events.push({ type: "echo", text: "\r\u001b[2K" });
      } else if (char >= " " || char === "\t") {
        this.append(char, events);
      }
    }
    return events;
  }

  private append(text: string, events: InputEvent[]): void {
    if (text.length === 0) return;
    if (this.buffer.length + text.length > MAX_BUFFER) return;
    this.buffer += text;
    events.push({ type: "echo", text: text.replace(/\r?\n/g, "\r\n") });
  }
}

/** How many trailing chars of `data` could begin `marker`. */
function partialSuffix(data: string, marker: string): number {
  for (let keep = Math.min(marker.length - 1, data.length); keep > 0; keep -= 1) {
    if (marker.startsWith(data.slice(data.length - keep))) return keep;
  }
  return 0;
}

/** Length of one escape sequence at the start of `data`; 0 if it is incomplete. */
function escapeLength(data: string): number {
  if (data.length < 2) return 0;
  if (data[1] === "[") {
    for (let i = 2; i < data.length; i += 1) {
      const code = data.charCodeAt(i);
      if (code >= 0x40 && code <= 0x7e) return i + 1;
    }
    return 0;
  }
  if (data[1] === "O") return data.length >= 3 ? 3 : 0;
  return 2;
}
