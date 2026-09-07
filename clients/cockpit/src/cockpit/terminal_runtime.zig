const std = @import("std");
const builtin = @import("builtin");
const native_sdk = @import("native_sdk");
const vt = @import("ghostty-vt");
const grid = @import("../terminal/grid.zig");
const local = @import("../providers/local/provider.zig");
const model_module = @import("model.zig");
const app_types = @import("app_types.zig");

const canvas = native_sdk.canvas;
const Model = model_module.Model;
pub const Pane = local.Pane;
pub const Fx = app_types.Fx;
/// `fx` is any effects instance with the pty verbs (`ptySpawn`, `ptyWrite`,
/// `ptyResize`, `ptyKill`), not only this app's own: the TypeScript-core
/// graph drives the same runtime from its adapter's effects, whose Msg type
/// is the compiled core's. `on_event` is that graph's own event constructor
/// for the same reason.
pub fn spawnPane(pane: *Pane, fx: anytype, on_event: anytype) void {
    const model = pane;
    model.session_generation +%= 1;
    if (model.session_generation == 0) model.session_generation = 1;
    model.phase = .starting;
    model.exit_code = 0;
    model.exit_signal = 0;
    model.exit_reason = .exited;
    // Leave selection mode: reset() clears the emulator's selection, so
    // a lingering `selecting` flag would show a caret over no selection
    // AND make the new shell reject all typed text until Escape.
    model.selecting = false;
    // The copy feedback belonged to the session that ended — the new
    // shell's status line must not claim its predecessor's clipboard.
    model.copied_bytes = 0;
    model.copy_failed = false;
    model.macos_natural_keys_held = 0;
    model.scrollback_wheel_accum = 0;
    model.mouse_wheel_y_accum = 0;
    model.mouse_wheel_x_accum = 0;
    model.mouse_wheel_next_horizontal = false;
    model.mouse_last_cell = null;
    model.mouse_protocol_fingerprint = 0;
    model.output_batches = 0;
    model.output_bytes = 0;
    // Drop any bytes still queued for the session that just ended — a
    // restarted shell must not receive the dead one's unsent keystrokes.
    model.outbound_head = 0;
    model.outbound_len = 0;
    model.outbound_dropped = 0;
    // Delivery accounting is generation-local.
    model.write_refusals = 0;
    model.write_refusals_total = 0;
    model.native_delivery_failures = 0;
    // Hard-reset the emulator so a restarted shell starts from a clean
    // terminal — no leftover mode (application-cursor, reverse video),
    // scrollback, palette override, or partial escape sequence from the
    // session that just ended. (A no-op on the first spawn.)
    model.session.reset();
    model.session.refreshScreenText();
    fx.ptySpawn(.{
        .key = model.pty_key,
        .argv = model.argv,
        .cols = model.cols,
        .rows = model.rows,
        .on_event = on_event,
    });
}

/// The pane owning a keyed pty event. An event for a key no pane holds
/// (a stale exit after a restart raced) is ignored rather than applied
/// to the wrong terminal.
pub fn paneForKey(model: *Model, key: u64) ?*Pane {
    return model.provider.terminalForPty(key);
}
/// Append outbound bytes (typed keys, pastes, or query replies) to the
/// pending ring in stream order, then flush what the pty's stdin FIFO
/// will take. A large payload is not submitted all at once: `flushOutbound`
/// paces it as the child reads, so the tail is never dropped. Admission
/// is ALL-OR-NOTHING — a query reply or encoded key cut mid-sequence
/// would feed the child a malformed control sequence, which is worse
/// than a whole loss — and the RESULT says which disposal the caller
/// must apply: `true` means the payload is DISPOSED (queued whole, or
/// impossible — larger than the ring itself — and counted as dropped);
/// `false` means it merely does not fit RIGHT NOW, is untouched and
/// uncounted, and the caller retains it to retry as the ring drains.
pub fn enqueueOutbound(model: *Pane, fx: anytype, bytes: []const u8) bool {
    const cap = model.outbound_buffer.len;
    if (bytes.len > cap) {
        model.outbound_dropped += bytes.len;
        return true;
    }
    if (bytes.len > cap - model.outbound_len) {
        // The occupancy may be STALE — the child may have resumed
        // reading since the ring filled — so drain what the FIFO will
        // take before refusing: a keystroke arriving between periodic
        // flushes must not drop when flushing would make room now.
        flushOutbound(model, fx);
        if (bytes.len > cap - model.outbound_len) return false;
    }
    for (bytes, 0..) |byte, i| {
        model.outbound_buffer[(model.outbound_head + model.outbound_len + i) % cap] = byte;
    }
    model.outbound_len += bytes.len;
    flushOutbound(model, fx);
    return true;
}

/// Enqueue a TRANSIENT payload (typed text, an encoded key): the event's
/// bytes do not outlive this dispatch, so a right-now refusal cannot be
/// retried later — it is counted as dropped instead, never silent.
/// Hitting this at all means the child ignored the whole 64 KiB ring.
///
/// STDIN ORDER comes first: a query reply retained behind a full ring is
/// OLDER than this keystroke and must reach the child before it. The
/// retained reply gets its retry now; if it still cannot enter the ring,
/// the keystroke must not jump the queue — it drops counted rather than
/// arrive before an answer the child may be parsing toward.
pub fn enqueueTransient(model: *Pane, fx: anytype, bytes: []const u8) void {
    moveResponsesToOutbound(model, fx);
    if (model.session.response_len > 0) {
        model.outbound_dropped += bytes.len;
        return;
    }
    if (!enqueueOutbound(model, fx, bytes)) {
        model.outbound_dropped += bytes.len;
    }
}

/// Push as much pending outbound as the pty's stdin FIFO will accept, in
/// per-write-bound chunks. `ptyWrite` reports acceptance — it alone knows
/// the byte- and record-ring limits — so a refused chunk stays in the
/// ring and is retried on the next output, resize, or frame: a
/// non-reading child pauses the stream instead of losing its tail, and a
/// reply is never removed before it actually lands.
pub fn flushOutbound(model: *Pane, fx: anytype) void {
    const cap = model.outbound_buffer.len;
    while (model.outbound_len > 0) {
        const run_to_end = cap - model.outbound_head;
        const n = @min(
            native_sdk.max_effect_pty_write_bytes,
            @min(model.outbound_len, run_to_end),
        );
        if (!fx.ptyWrite(model.pty_key, model.outbound_buffer[model.outbound_head .. model.outbound_head + n])) {
            model.write_refusals +|= 1;
            model.write_refusals_total +|= 1;
            break;
        }
        model.write_refusals = 0;
        model.outbound_head = (model.outbound_head + n) % cap;
        model.outbound_len -= n;
    }
}

/// Feed one pty output batch and return the emulator's query answers to
/// the child. A batch can be many times the response buffer, and a
/// pathological all-query batch (thousands of pipelined DSR/DA1 requests)
/// could produce more replies than the buffer holds in one pass — so the
/// batch is fed in sub-slices no larger than the response buffer, with
/// the answers drained after each. The VT stream keeps parser state
/// across slices, so splitting mid-escape-sequence is invisible; each
/// query's reply is well under a slice's worth of input, so the buffer
/// never overflows and no reply is dropped. This keeps the write-back
/// lossless: a child that blocks on a DSR answer never hangs.
pub fn feedOutput(model: *Pane, fx: anytype, bytes: []const u8) void {
    const slice_bytes = grid.Session.feed_slice_bytes;
    var offset: usize = 0;
    while (offset < bytes.len) {
        const end = @min(offset + slice_bytes, bytes.len);
        model.session.feed(bytes[offset..end]);
        moveResponsesToOutbound(model, fx);
        offset = end;
    }
    // A zero-length batch never reaches here (the engine coalesces only
    // non-empty reads), but a batch that produced no output still drains
    // any answer a prior partial sequence completed.
    if (bytes.len == 0) moveResponsesToOutbound(model, fx);
}

/// Move the emulator's query answers (DSR, DA1, ...) into the pending
/// outbound ring, in stream order after whatever input preceded them,
/// then flush. Routing them through the SAME ring as typed input is what
/// makes them lossless: a reply refused by a full FIFO stays queued and
/// retries, never cleared before it lands (which would hang a child
/// blocking on it). Replies are DURABLE (the emulator's buffer holds
/// them), so a ring too full right now leaves them IN PLACE — uncleared,
/// retried on the next output, resize, or frame — instead of discarding
/// an answer the child may be blocked on. Only a queued (or impossible,
/// counted) batch clears; never a torn escape sequence either way.
pub fn moveResponsesToOutbound(model: *Pane, fx: anytype) void {
    const pending = model.session.pendingResponses();
    if (pending.len > 0) {
        if (!enqueueOutbound(model, fx, pending)) return;
    }
    model.session.clearResponses();
}
/// Encode one key transition and push the bytes toward the child. macOS
/// natural-text arrow gestures use conventional shell bindings;
/// everything else goes through the emulator's encoder. Releases ride
/// the same path with `.release`: the encoder emits them only under the
/// kitty protocol's negotiated event reporting and stays silent in
/// legacy modes. (Key REPEAT is the one event type the hosts do not
/// distinguish from a fresh press, so a TUI that enabled event reporting
/// sees repeats as presses.)
pub fn encodeKeyEvent(model: *Pane, fx: anytype, event: canvas.WidgetKeyboardEvent, action: vt.input.KeyAction) void {
    const session = model.session;
    const natural_key_mask = macosNaturalTextKeyMask(event.key);
    if (action == .release and natural_key_mask != 0 and
        (model.macos_natural_keys_held & natural_key_mask) != 0)
    {
        model.macos_natural_keys_held &= ~natural_key_mask;
        return;
    }
    // A fresh press supersedes a stale latch, then re-arms it below when
    // this is another natural-editing gesture (auto-repeat included).
    if (action == .press and natural_key_mask != 0) {
        model.macos_natural_keys_held &= ~natural_key_mask;
    }
    if (macosNaturalTextSequence(event)) |sequence| {
        // Natural-text bindings consume the whole gesture. In
        // particular, a child using kitty event reporting must not
        // receive a release for a modified arrow whose press arrived as
        // legacy editing bytes.
        if (action == .release) return;
        model.macos_natural_keys_held |= natural_key_mask;
        session.scrollToBottom();
        enqueueTransient(model, fx, sequence);
        return;
    }
    const mods = event.modifiers;
    const key = mapKey(event) orelse blk: {
        // A release of a plain printable never maps (its PRESS came
        // through the text channel): synthesize the codepoint-keyed
        // event so kitty event reporting hears the release too.
        if (action != .release) return;
        break :blk mapPrintable(event.key) orelse return;
    };
    var buffer: [128]u8 = undefined;
    var writer: std.Io.Writer = .fixed(&buffer);
    const encode_options: vt.input.KeyEncodeOptions = .fromTerminal(&session.term);
    // The runtime folds the platform's PRIMARY modifier into `super`.
    // On macOS primary IS the GUI key, so the fold is harmless there —
    // but on hosts whose primary is Ctrl, a bare Ctrl chord arrives as
    // ctrl+super and the encoder would skip its C0 byte (Ctrl+C must
    // deliver ETX and interrupt the child, never a CSI-u chord). Undo
    // the alias for the encoder: super counts only when Ctrl is not the
    // key raising it. (The one loss is the GUI+Ctrl double chord, which
    // encodes as plain Ctrl — the convention terminals follow anyway.)
    const encoder_super = mods.super and !mods.control;
    _ = vt.input.encodeKey(&writer, .{
        .key = key.key,
        .action = action,
        .mods = .{
            .shift = mods.shift,
            .ctrl = mods.control,
            .alt = mods.alt,
            .super = encoder_super,
        },
        .utf8 = key.utf8,
        .unshifted_codepoint = key.unshifted,
    }, encode_options) catch return;
    if (writer.end == 0) return;
    session.scrollToBottom();
    // Through the pending ring like committed text, so an encoded key
    // typed while a paste is still draining lands after it in the stream.
    enqueueTransient(model, fx, buffer[0..writer.end]);
}

fn macosNaturalTextKeyMask(key: []const u8) u8 {
    if (comptime builtin.os.tag != .macos) return 0;
    if (keyIs(key, "arrowleft")) return 1 << 0;
    if (keyIs(key, "arrowright")) return 1 << 1;
    if (keyIs(key, "backspace")) return 1 << 2;
    return 0;
}

/// Match macOS terminals' "natural text editing" bindings. These are
/// exact bare-modifier gestures: shifted or combined chords continue
/// through the key encoder so terminal applications can distinguish
/// them. The raw bindings intentionally bypass negotiated kitty
/// reporting, just as Ghostty's own default keybinds do.
fn macosNaturalTextSequence(event: canvas.WidgetKeyboardEvent) ?[]const u8 {
    if (comptime builtin.os.tag != .macos) return null;
    const mods = event.modifiers;
    if (mods.shift or mods.control) return null;
    if (mods.alt and !mods.super) {
        if (keyIs(event.key, "arrowleft")) return "\x1bb";
        if (keyIs(event.key, "arrowright")) return "\x1bf";
    }
    if (mods.super and !mods.alt) {
        if (keyIs(event.key, "arrowleft")) return "\x01";
        if (keyIs(event.key, "arrowright")) return "\x05";
        // The physical macOS Delete key is normalized as Backspace.
        if (keyIs(event.key, "backspace")) return "\x15";
    }
    return null;
}

/// Committed text reaches the child through the emulator's key encoder
/// when it is a single scalar: byte-identical to the raw text under
/// legacy modes (the encoder writes unmodified text through untouched)
/// and the negotiated CSI-u form when a TUI enabled the kitty
/// protocol's report-all mode — raw bytes there would desynchronize the
/// application's key decoding. Multi-scalar commits (IME words, paste)
/// stay raw text, the protocol's rule for composed input.
pub fn sendCommittedText(model: *Pane, fx: anytype, text: []const u8) void {
    single: {
        const len = std.unicode.utf8ByteSequenceLength(text[0]) catch break :single;
        if (text.len != len) break :single;
        const cp = std.unicode.utf8Decode(text[0..len]) catch break :single;
        var buffer: [128]u8 = undefined;
        var writer: std.Io.Writer = .fixed(&buffer);
        _ = vt.input.encodeKey(&writer, .{
            .key = .unidentified,
            .action = .press,
            .utf8 = text,
            .unshifted_codepoint = cp,
        }, .fromTerminal(&model.session.term)) catch break :single;
        if (writer.end == 0) break :single;
        enqueueTransient(model, fx, buffer[0..writer.end]);
        return;
    }
    enqueueTransient(model, fx, text);
}

pub fn keyIs(key: []const u8, name: []const u8) bool {
    return std.ascii.eqlIgnoreCase(key, name);
}

const MappedKey = struct {
    key: vt.input.Key,
    utf8: []const u8 = "",
    unshifted: u21 = 0,
};

/// The same native key mapping for coordinator-backed terminals. Printable
/// presses belong to committed text; forwarding both would type twice.
pub fn providerKey(event: canvas.WidgetKeyboardEvent) ?@import("provider_contract").KeyInput {
    const release = event.phase == .key_up;
    const mapped = mapKey(event) orelse blk: {
        if (!release) return null;
        break :blk mapPrintable(event.key) orelse return null;
    };
    return .{
        .action = if (release) .release else .press,
        .physical = @enumFromInt(@intFromEnum(mapped.key)),
        .text = if (release) "" else mapped.utf8,
        .unshifted_codepoint = mapped.unshifted,
        .modifiers = .{
            .shift = event.modifiers.shift,
            .control = event.modifiers.control,
            .alt = event.modifiers.alt,
            .super = event.modifiers.super and !event.modifiers.control,
        },
    };
}

/// Host key names -> emulator key codes, for keys that do not commit
/// A plain printable's codepoint-keyed event, for RELEASE encoding
/// only: its press travels the committed-text channel, but kitty event
/// reporting still owes the child the release of the same key. No text
/// rides a release.
fn mapPrintable(key: []const u8) ?MappedKey {
    if (key.len == 0) return null;
    const len = std.unicode.utf8ByteSequenceLength(key[0]) catch return null;
    if (key.len != len) return null;
    const cp = std.unicode.utf8Decode(key[0..len]) catch return null;
    return .{ .key = .unidentified, .unshifted = cp };
}

/// text (specials always; letters/digits only under a chord modifier,
/// where the text channel stays silent and the encoder must speak).
fn mapKey(event: canvas.WidgetKeyboardEvent) ?MappedKey {
    const key = event.key;
    const specials = [_]struct { name: []const u8, key: vt.input.Key }{
        .{ .name = "enter", .key = .enter },
        .{ .name = "tab", .key = .tab },
        .{ .name = "escape", .key = .escape },
        .{ .name = "backspace", .key = .backspace },
        .{ .name = "delete", .key = .delete },
        .{ .name = "arrowup", .key = .arrow_up },
        .{ .name = "arrowdown", .key = .arrow_down },
        .{ .name = "arrowleft", .key = .arrow_left },
        .{ .name = "arrowright", .key = .arrow_right },
        .{ .name = "home", .key = .home },
        .{ .name = "end", .key = .end },
        .{ .name = "pageup", .key = .page_up },
        .{ .name = "pagedown", .key = .page_down },
        .{ .name = "insert", .key = .insert },
        // Function keys produce no committed text, so the encoder must
        // build their escape sequences or the child never sees them.
        .{ .name = "f1", .key = .f1 },
        .{ .name = "f2", .key = .f2 },
        .{ .name = "f3", .key = .f3 },
        .{ .name = "f4", .key = .f4 },
        .{ .name = "f5", .key = .f5 },
        .{ .name = "f6", .key = .f6 },
        .{ .name = "f7", .key = .f7 },
        .{ .name = "f8", .key = .f8 },
        .{ .name = "f9", .key = .f9 },
        .{ .name = "f10", .key = .f10 },
        .{ .name = "f11", .key = .f11 },
        .{ .name = "f12", .key = .f12 },
    };
    for (specials) |entry| {
        if (keyIs(key, entry.name)) return .{ .key = entry.key };
    }
    // Chorded character keys (ctrl+c, alt+f, ...): the text channel is
    // silent for these, so the encoder builds the control sequence.
    // Alt is a chord EXCEPT on macOS, where Option is a compose key —
    // Option+F commits the composed `ƒ` through the text channel, so
    // encoding an Alt-F escape here too would double the input (the
    // child would see both). On macOS, Option composes; everywhere else
    // Alt is Meta (ESC prefix, no composed text) — EXCEPT Ctrl+Alt
    // together ON WINDOWS, which is how that host represents AltGr:
    // the combination composes text there (AltGr+Q commits `@` through
    // the text channel), so encoding it as a chord would send wrong
    // bytes AND shadow the composed character. Linux keeps Ctrl+Alt as
    // a genuine chord — its AltGr is a distinct modifier that never
    // reports as ctrl+alt, so Ctrl+Alt+C must still encode.
    const altgr = event.modifiers.control and event.modifiers.alt and builtin.os.tag == .windows;
    const alt_is_chord = event.modifiers.alt and builtin.os.tag != .macos;
    const chorded = (event.modifiers.control or event.modifiers.super or alt_is_chord) and !altgr;
    if (!chorded) return null;
    if (key.len == 1) {
        // The emulator's encoder expects the pressed CHARACTER as UTF-8
        // alongside the logical key — the shape its host normally
        // supplies — and derives the chord bytes from it: legacy C0
        // sequences (Ctrl+C -> 0x03, Ctrl+\ -> 0x1C) where they exist,
        // and the fixterms CSI-u encoding for the exceptions (Ctrl+[,
        // Ctrl+I, Ctrl+M keep their unchorded bytes unambiguous).
        const ch = key[0];
        const utf8 = key[0..1];
        if (ch >= 'a' and ch <= 'z') {
            const base = @intFromEnum(vt.input.Key.key_a);
            return .{
                .key = @enumFromInt(base + @as(c_int, ch - 'a')),
                .utf8 = utf8,
                .unshifted = ch,
            };
        }
        if (ch >= '0' and ch <= '9') {
            const base = @intFromEnum(vt.input.Key.digit_0);
            return .{
                .key = @enumFromInt(base + @as(c_int, ch - '0')),
                .utf8 = utf8,
                .unshifted = ch,
            };
        }
        // Chorded punctuation carries real control meaning a terminal
        // user expects — Ctrl+[ is the ESC chord, Ctrl+\ is SIGQUIT,
        // Ctrl+] exits telnet — and has no text-channel fallback, so an
        // unmapped key here is silently lost input.
        const punctuation = [_]struct { ch: u8, key: vt.input.Key }{
            .{ .ch = '[', .key = .bracket_left },
            .{ .ch = ']', .key = .bracket_right },
            .{ .ch = '\\', .key = .backslash },
            .{ .ch = ';', .key = .semicolon },
            .{ .ch = '\'', .key = .quote },
            .{ .ch = ',', .key = .comma },
            .{ .ch = '.', .key = .period },
            .{ .ch = '/', .key = .slash },
            .{ .ch = '-', .key = .minus },
            .{ .ch = '=', .key = .equal },
            .{ .ch = '`', .key = .backquote },
        };
        for (punctuation) |entry| {
            if (ch == entry.ch) return .{ .key = entry.key, .utf8 = utf8, .unshifted = entry.ch };
        }
    }
    if (keyIs(key, "space")) return .{ .key = .space, .utf8 = " ", .unshifted = ' ' };
    return null;
}
