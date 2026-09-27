const std = @import("std");
const builtin = @import("builtin");
const native_sdk = @import("native_sdk");
const vt = @import("ghostty-vt");
const grid = @import("../terminal/grid.zig");
const local = @import("../providers/local/provider.zig");
const model_module = @import("model.zig");

const canvas = native_sdk.canvas;
const Model = model_module.Model;
pub const Pane = local.Pane;
/// `fx` is any effects value with the pty verbs; `on_event` is its pty event
/// constructor.
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
    // A restarted shell starts from a clean emulator.
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

/// A terminal event ends this generation's delivery obligation. Retaining
/// queued input after a failed spawn both misreports loss and risks replaying
/// it into a later process. Shipping and retained reducers share this policy.
pub fn finishSession(pane: *Pane, event: native_sdk.EffectPtyEvent) void {
    pane.phase = if (event.reason == .rejected or event.reason == .spawn_failed) .failed else .ended;
    pane.exit_code = event.code;
    pane.exit_signal = event.signal;
    pane.exit_reason = event.reason;
    pane.native_delivery_failures = event.dropped_writes -| pane.write_refusals_total;
    pane.write_refusals = 0;
    pane.outbound_dropped += pane.outbound_len + pane.session.pendingResponses().len;
    pane.outbound_head = 0;
    pane.outbound_len = 0;
    pane.session.clearResponses();
    pane.macos_natural_keys_held = 0;
}

/// Local work is demand-driven, independent of visible frames or child output.
/// Each pane has a bounded outbound ring and the search engine's existing
/// per-slice budget. Failed panes remain searchable, but never write again.
pub fn maintenancePending(model: *const Model) bool {
    for (0..model_module.max_terminals) |index| {
        if (model.provider.states[index] != .active) continue;
        if (paneMaintenancePending(model.provider.slotConst(index))) return true;
    }
    return false;
}

fn paneMaintenancePending(pane: *const Pane) bool {
    if (pane.session.searchPending()) return true;
    if (!pane.acceptsInput()) return false;
    return pane.outbound_len > 0 or pane.session.response_len > 0;
}

pub fn drainPane(pane: *Pane, fx: anytype) void {
    if (!pane.acceptsInput()) return;
    flushOutbound(pane, fx);
    moveResponsesToOutbound(pane, fx);
}

pub fn maintainPane(pane: *Pane, fx: anytype) void {
    drainPane(pane, fx);
    _ = pane.session.searchPump(grid.Session.search_frame_slice_steps);
}

/// Append outbound bytes to the pending ring in stream order and flush what
/// the pty will take. Admission is all-or-nothing (never a torn escape
/// sequence). True: disposed (queued, or larger than the ring and counted
/// dropped). False: does not fit yet; the caller keeps it and retries.
pub fn enqueueOutbound(model: *Pane, fx: anytype, bytes: []const u8) bool {
    const cap = model.outbound_buffer.len;
    if (bytes.len > cap) {
        model.outbound_dropped += bytes.len;
        return true;
    }
    if (bytes.len > cap - model.outbound_len) {
        // Occupancy may be stale; flush before refusing.
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

/// Enqueue a transient payload (typed text, an encoded key) that cannot be
/// retried later: a refusal is counted as dropped. Retained query replies are
/// older and go first; if they still cannot enter, the keystroke drops rather
/// than jump the queue.
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

/// Push pending outbound in chunks as far as `ptyWrite` accepts; a refused
/// chunk stays queued for maintenance to retry, so nothing is lost.
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

/// Feed one pty output batch in `feed_slice_bytes` sub-slices, draining query
/// answers after each so a pipelined burst of queries never overflows the
/// response buffer (a child blocked on a DSR answer must never hang).
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

/// Move query answers into the outbound ring after preceding input, then
/// flush. When the ring is full they stay in the emulator's buffer to retry;
/// only a queued (or impossible, counted) batch is cleared.
pub fn moveResponsesToOutbound(model: *Pane, fx: anytype) void {
    const pending = model.session.pendingResponses();
    if (pending.len > 0) {
        if (!enqueueOutbound(model, fx, pending)) return;
    }
    model.session.clearResponses();
}
/// Encode one key transition toward the child: macOS natural-text gestures
/// use shell bindings, everything else the emulator's encoder (which emits
/// releases only under kitty event reporting). Repeats arrive as presses.
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
        // Natural-text bindings consume the whole gesture, releases included.
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
    // The runtime folds PRIMARY into `super`; where primary is Ctrl, a bare
    // Ctrl chord would lose its C0 byte (Ctrl+C must send ETX). Super counts
    // only without Ctrl.
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

pub fn macosNaturalTextKeyMask(key: []const u8) u8 {
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

/// Single-scalar committed text goes through the key encoder (raw in legacy
/// modes, CSI-u under kitty report-all); multi-scalar commits stay raw text.
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

/// Apply the same native editing policy to a structured remote key. The
/// coordinator retains control of its negotiated terminal-key encoding.
pub fn providerNaturalKey(event: canvas.WidgetKeyboardEvent) ?@import("provider_contract").KeyInput {
    const sequence = macosNaturalTextSequence(event) orelse return null;
    const bindings = .{
        .{ "\x1bb", vt.input.Key.key_b, "b", true },
        .{ "\x1bf", vt.input.Key.key_f, "f", true },
        .{ "\x01", vt.input.Key.key_a, "a", false },
        .{ "\x05", vt.input.Key.key_e, "e", false },
        .{ "\x15", vt.input.Key.key_u, "u", false },
    };
    inline for (bindings) |binding| {
        if (std.mem.eql(u8, sequence, binding[0])) return .{
            .action = .press,
            .physical = @enumFromInt(@intFromEnum(binding[1])),
            .text = binding[2],
            .unshifted_codepoint = binding[2][0],
            .modifiers = .{ .alt = binding[3], .control = !binding[3] },
        };
    }
    return null;
}

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

/// A printable's codepoint-keyed event for release encoding only (its press
/// travels as committed text).
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
    // Chorded character keys have no text, so the encoder builds them. On
    // macOS Option composes text (not a chord); on Windows Ctrl+Alt is AltGr
    // and composes too; elsewhere Alt is Meta.
    const altgr = event.modifiers.control and event.modifiers.alt and builtin.os.tag == .windows;
    const alt_is_chord = event.modifiers.alt and builtin.os.tag != .macos;
    const chorded = (event.modifiers.control or event.modifiers.super or alt_is_chord) and !altgr;
    if (!chorded) return null;
    if (key.len == 1) {
        // The encoder derives chord bytes (C0 or CSI-u) from the character.
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
        // Chorded punctuation (Ctrl+[, Ctrl+\, Ctrl+]) has no text fallback.
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
