//! Provider-qualified interaction shared by native event entry points.
//! Clipboard completions belong to the replica that requested them, not focus.
const std = @import("std");
const native_sdk = @import("native_sdk");
const contract = @import("provider_contract");
const model_module = @import("model.zig");
const local = @import("../providers/local/provider.zig");
const runtime = @import("terminal_runtime.zig");
const support = @import("phux_support.zig");
const vt = @import("ghostty-vt");
const remote_commands = @import("native/remote_presentation_commands.zig");
const paste_safety = @import("paste_safety.zig");
const projection = @import("native/workspace_projection.zig");

const Model = model_module.Model;
const TerminalRef = contract.TerminalRef;
const Event = native_sdk.canvas.WidgetKeyboardEvent;

/// Captured work must not reacquire UI state through the currently focused
/// attachment when another client presents the same terminal reference.
pub fn stateForOwner(model: *Model, owner: contract.ReplicaOwner) ?*model_module.RemoteUiState {
    return @constCast(stateForOwnerConst(model, owner));
}

pub fn stateForOwnerConst(model: *const Model, owner: contract.ReplicaOwner) ?*const model_module.RemoteUiState {
    for (&model.remote_ui) |*state| {
        if (state.terminal_ref == null) continue;
        if (state.owner.eql(owner)) return state;
    }
    return null;
}

pub fn presentationForOwner(model: *const Model, owner: contract.ReplicaOwner) ?contract.Presentation {
    const remote = model.phuxForOwnerConst(owner) orelse return null;
    const current = remote.owner(owner.terminal_ref) orelse return null;
    if (!current.eql(owner)) return null;
    const presentation = remote.presentation(owner.terminal_ref) orelse return null;
    return if (presentation.owner.eql(owner)) presentation else null;
}

pub fn copy(model: *Model, fx: anytype, ref: TerminalRef) void {
    const owner = model.terminalOwner(ref) orelse return;
    copyForOwner(model, fx, owner);
}

/// Start a clipboard write for the replica captured by a native menu. No
/// ref-wide lookup is allowed here: equal refs may belong to sibling clients.
pub fn copyForOwner(model: *Model, fx: anytype, owner: contract.ReplicaOwner) void {
    if (model.copy_inflight) return;
    const text = selectionTextForOwner(model, owner) catch {
        copyFailed(model, owner);
        return;
    };
    defer std.heap.page_allocator.free(text);
    if (text.len == 0) return;
    model.copy_owner = owner;
    model.copy_inflight = true;
    fx.writeClipboard(.{ .key = local.clipboard_key, .text = text });
}

fn selectionTextForOwner(model: *Model, owner: contract.ReplicaOwner) ![]u8 {
    const ref = owner.terminal_ref;
    if (contract.isLocal(ref)) return localSelectionText(model, owner);
    return remoteSelectionTextForOwner(model, owner);
}

fn localSelectionText(model: *Model, owner: contract.ReplicaOwner) ![]u8 {
    if (!model.provider.ownerIsCurrent(owner)) return error.StaleOwner;
    const pane = model.provider.terminal(owner.terminal_ref) orelse return error.NoSelection;
    pane.copy_failed = false;
    const text = try pane.session.selectionText(std.heap.page_allocator);
    const result = text orelse return error.NoSelection;
    defer std.heap.page_allocator.free(result);
    pane.copied_bytes = result.len;
    return std.heap.page_allocator.dupe(u8, result);
}

fn remoteSelectionTextForOwner(model: *Model, owner: contract.ReplicaOwner) ![]u8 {
    const ref = owner.terminal_ref;
    const remote = model.phuxForOwner(owner) orelse return error.NoProvider;
    const current = remote.owner(ref) orelse return error.StaleOwner;
    if (!current.eql(owner)) return error.StaleOwner;
    const state = stateForOwner(model, owner) orelse return error.NoSelection;
    state.copy_failed = false;
    const text = try remoteSelectionText(model, remote, state);
    state.copied_bytes = text.len;
    return text;
}

fn remoteSelectionText(model: *Model, remote: *@import("phux_support.zig").PhuxProvider, state: *model_module.RemoteUiState) ![]u8 {
    // A pointer selection or Select All takes precedence over the search match.
    // Reacquire search anchors only when another pane's query retired this range.
    const text = remote.selectionText(state.owner, std.heap.page_allocator) catch return recoverSearchSelection(model, remote, state);
    if (text.len != 0 or !state.search.open) return text;
    std.heap.page_allocator.free(text);
    return recoverSearchSelection(model, remote, state);
}

fn recoverSearchSelection(model: *Model, remote: *@import("phux_support.zig").PhuxProvider, state: *model_module.RemoteUiState) ![]u8 {
    if (!remote_commands.prepareCopy(model, state)) return error.NoSelection;
    return remote.selectionText(state.owner, std.heap.page_allocator);
}

fn copyFailed(model: *Model, owner: contract.ReplicaOwner) void {
    if (model.provider.terminal(owner.terminal_ref)) |pane| {
        pane.copy_failed = true;
        pane.copied_bytes = 0;
        return;
    }
    const state = stateForOwner(model, owner) orelse return;
    state.copy_failed = true;
    state.copied_bytes = 0;
}

pub fn copied(model: *Model, ok: bool) void {
    if (!model.copy_inflight) return;
    model.copy_inflight = false;
    if (!clipboardOwnerCurrent(model, model.copy_owner)) return;
    const ref = model.copy_owner.terminal_ref;
    if (!ok) return copyFailed(model, model.copy_owner);
    if (model.provider.terminal(ref)) |pane| {
        pane.selecting = false;
        return;
    }
    const state = stateForOwner(model, model.copy_owner) orelse return;
    state.selecting = false;
}

pub fn requestPaste(model: *Model, fx: anytype, ref: TerminalRef) void {
    const owner = model.terminalOwner(ref) orelse return;
    requestPasteForOwner(model, fx, owner);
}

/// Start a clipboard read for the exact captured replica. The completion is
/// already owner-qualified by `paste_owner` and follows this same route.
pub fn requestPasteForOwner(model: *Model, fx: anytype, owner: contract.ReplicaOwner) void {
    if (model.paste_inflight) return;
    // A new paste supersedes one still waiting on its answer.
    _ = cancelPendingPaste(model);
    if (!acceptsPasteForOwner(model, owner)) {
        model.paste_failed = true;
        return;
    }
    model.paste_owner = owner;
    model.paste_failed = false;
    model.paste_inflight = true;
    fx.readClipboard(.{ .key = local.paste_clipboard_key });
}

fn acceptsPasteForOwner(model: *Model, owner: contract.ReplicaOwner) bool {
    const ref = owner.terminal_ref;
    model.paste_target = .terminal;
    if (contract.isLocal(ref)) {
        if (!model.provider.ownerIsCurrent(owner)) return false;
        const pane = model.provider.terminal(ref) orelse return false;
        if (pane.session.search.open) {
            model.paste_target = .search_needle;
            return true;
        }
        return pane.acceptsInput();
    }
    const presentation = presentationForOwner(model, owner) orelse return false;
    const state = stateForOwner(model, owner) orelse return false;
    if (state.search.open) {
        model.paste_target = .search_needle;
        state.search.paste_pending = true;
        return true;
    }
    return presentation.phase == .live;
}

/// Menu policy for one captured owner. A live mouse-reporting TUI keeps
/// secondary click; a retained local snapshot may expose an existing selection.
pub fn clipboardEnabledForOwner(model: *Model, owner: contract.ReplicaOwner, action: anytype) bool {
    const ref = owner.terminal_ref;
    if (contract.isLocal(ref)) return localClipboardEnabled(model, owner, action);
    return remoteClipboardEnabled(model, owner, action);
}

fn localClipboardEnabled(model: *Model, owner: contract.ReplicaOwner, action: anytype) bool {
    if (!model.provider.ownerIsCurrent(owner)) return false;
    const pane = model.provider.terminal(owner.terminal_ref) orelse return false;
    if (@import("pointer_input.zig").paneReportsMouse(pane)) return false;
    return switch (action) {
        .copy => pane.session.selectionActive(),
        .paste => pane.acceptsInput(),
    };
}

fn remoteClipboardEnabled(model: *Model, owner: contract.ReplicaOwner, action: anytype) bool {
    const ref = owner.terminal_ref;
    const remote = model.phuxForOwner(owner) orelse return false;
    const current = remote.owner(ref) orelse return false;
    if (!current.eql(owner)) return false;
    const presentation = remote.presentation(ref) orelse return false;
    if (!presentation.owner.eql(owner)) return false;
    if (!remoteClipboardMenuAvailable(remote, owner, presentation.phase)) return false;
    return switch (action) {
        .copy => hasSelection(model, owner, presentation),
        .paste => presentation.phase == .live,
    };
}

fn remoteClipboardMenuAvailable(remote: anytype, owner: contract.ReplicaOwner, phase: contract.Phase) bool {
    if (phase != .live) return false;
    return !(remote.mouseTracking(owner) catch return false);
}

/// Menu projection must stay constant-time. The grid answers for an on-screen
/// range; owner-qualified handles retain a range that has scrolled off-screen.
/// Search owns borrowed result handles rather than storing them in this state.
fn hasSelection(model: *const Model, owner: contract.ReplicaOwner, presentation: contract.Presentation) bool {
    if (presentation.grid.selection_active) return true;
    const state = stateForOwnerConst(model, owner) orelse return false;
    if (state.start_anchor != 0 and state.end_anchor != 0 and state.start_anchor != state.end_anchor) return true;
    return state.search.open and state.search.count != 0;
}

fn clipboardOwnerCurrent(model: *const Model, owner: contract.ReplicaOwner) bool {
    if (contract.isLocal(owner.terminal_ref)) return model.provider.ownerIsCurrent(owner);
    const remote = model.phuxForOwnerConst(owner) orelse return false;
    const current = remote.owner(owner.terminal_ref) orelse return false;
    if (!current.eql(owner)) return false;
    const presentation = remote.presentation(owner.terminal_ref) orelse return false;
    return presentation.owner.eql(owner) and presentation.phase == .live;
}

pub fn pasted(model: *Model, fx: anytype, ok: bool, text: []const u8) void {
    if (!model.paste_inflight) return;
    model.paste_inflight = false;
    if (!model.ownerIsCurrent(model.paste_owner)) return;
    model.paste_failed = !ok;
    if (!ok) return;
    if (model.provider.terminal(model.paste_owner.terminal_ref)) |pane| {
        pasteLocal(model, pane, fx, text);
        return;
    }
    if (model.paste_target == .search_needle) {
        pasteRemoteSearch(model, text);
        return;
    }
    if (holdUnsafePaste(model, model.paste_owner, text)) return;
    sendRemotePaste(model, model.paste_owner, text);
}

/// The user's own clipboard paste, already past Cockpit's paste protection,
/// is trusted: the server brackets it by the pane's DEC 2004 mode and does
/// not refuse a multi-line paste the user chose (as `phux paste` and the
/// TUI's DEC 2004 path). Untrusted, the server would drop every multi-line
/// paste, bracketed or not, before the user could say yes.
fn sendRemotePaste(model: *Model, owner: contract.ReplicaOwner, text: []const u8) void {
    const remote = model.phuxForOwner(owner) orelse {
        model.paste_failed = true;
        return;
    };
    remote.sendPaste(owner, text, true) catch {
        model.paste_failed = true;
    };
}

/// Whether the receiver has DEC 2004 on. A Phux replica that cannot answer
/// counts as unbracketed, the side that asks.
fn bracketedFor(model: *const Model, owner: contract.ReplicaOwner) bool {
    if (model.provider.terminalConst(owner.terminal_ref)) |pane| {
        return vt.input.PasteOptions.fromTerminal(&pane.session.term).bracketed;
    }
    const remote = model.phuxForOwnerConst(owner) orelse return false;
    return remote.bracketedPaste(owner) catch false;
}

/// Hold a paste the receiver would not see as one (paste_safety.zig) for the
/// user's answer. Returns whether it was held; a held paste reaches nothing
/// until `confirmPendingPaste`.
fn holdUnsafePaste(model: *Model, owner: contract.ReplicaOwner, text: []const u8) bool {
    const at_prompt = projection.terminalAtPrompt(model, owner.terminal_ref);
    const receiver = switch (paste_safety.assess(text, bracketedFor(model, owner), at_prompt)) {
        .deliver => return false,
        .confirm => |value| value,
    };
    const copy_text = std.heap.page_allocator.dupe(u8, text) catch {
        // Unable to hold it is unable to ask: refuse rather than deliver.
        model.paste_failed = true;
        return true;
    };
    _ = cancelPendingPaste(model);
    model.paste_pending = .{ .owner = owner, .text = copy_text, .lines = paste_safety.lineCount(text), .receiver = receiver };
    return true;
}

/// The held paste, when it still belongs to a current replica.
pub fn pendingPaste(model: *const Model) ?*const model_module.PendingPaste {
    return model.pendingPaste();
}

/// Drop the held paste unsent. Returns whether one was held.
pub fn cancelPendingPaste(model: *Model) bool {
    const pending = model.paste_pending orelse return false;
    model.paste_pending = null;
    std.heap.page_allocator.free(pending.text);
    return true;
}

/// Deliver the held paste to exactly the replica it was read for, or to
/// nothing when that replica is gone or no longer takes input.
pub fn confirmPendingPaste(model: *Model, fx: anytype) bool {
    const pending = model.paste_pending orelse return false;
    model.paste_pending = null;
    defer std.heap.page_allocator.free(pending.text);
    if (!model.ownerIsCurrent(pending.owner)) return true;
    model.paste_owner = pending.owner;
    model.paste_failed = false;
    if (model.provider.terminal(pending.owner.terminal_ref)) |pane| {
        if (!pane.acceptsInput()) {
            model.paste_failed = true;
            return true;
        }
        pasteClipboardText(model, pane, fx, pending.text);
        return true;
    }
    if (presentationForOwner(model, pending.owner)) |presentation| {
        if (presentation.phase == .live) {
            sendRemotePaste(model, pending.owner, pending.text);
            return true;
        }
    }
    model.paste_failed = true;
    return true;
}

fn pasteRemoteSearch(model: *Model, text: []const u8) void {
    const state = stateForOwner(model, model.paste_owner) orelse return;
    const pending = state.search.paste_pending;
    state.search.paste_pending = false;
    if (!pending) return;
    model.paste_failed = !remote_commands.pasteForOwner(model, model.paste_owner, text);
}

fn pasteLocal(model: *Model, pane: *local.Pane, fx: anytype, text: []const u8) void {
    if (model.paste_target == .search_needle) {
        model.paste_failed = !pane.session.searchPaste(text);
        return;
    }
    if (!pane.acceptsInput()) {
        model.paste_failed = true;
        return;
    }
    if (holdUnsafePaste(model, model.paste_owner, text)) return;
    pasteClipboardText(model, pane, fx, text);
}

pub fn resize(model: *Model, fx: anytype, ref: TerminalRef, viewport: contract.Viewport) void {
    if (model.provider.terminal(ref)) |pane| {
        resizeLocal(pane, fx, viewport);
        return;
    }
    // Each coordinator's terminal is sized on its own server, only once a
    // pane shows it and it is live.
    const owner = model.terminalOwner(ref) orelse return;
    resizeRemote(model, owner, viewport);
}

pub fn resizeRemote(model: *Model, owner: contract.ReplicaOwner, viewport: contract.Viewport) void {
    const remote = model.phuxForOwner(owner) orelse return;
    if (!model.ownerIsCurrent(owner)) return;
    // The host keeps failed/not-ready submissions retryable at its next
    // readiness drain; an unchanged authoritative grid is not a retry signal.
    remote.viewportResize(owner.terminal_ref, viewport) catch {};
}

fn resizeLocal(pane: *local.Pane, fx: anytype, viewport: contract.Viewport) void {
    if (pane.cols == viewport.cols and pane.rows == viewport.rows) return;
    if (!pane.session.resize(viewport.cols, viewport.rows)) return;
    // phux-cockpit-pg1 diagnostic, paired with `terminal spawn`: the grid
    // the shell is about to be told, and where the emulator's cursor landed
    // after the reflow, so a stranded cursor can be traced to its resize.
    std.log.info("terminal resize pty={d} grid={d}x{d} -> {d}x{d} cursor_row={d}", .{
        pane.pty_key,
        pane.cols,
        pane.rows,
        viewport.cols,
        viewport.rows,
        pane.session.term.screens.active.cursor.y,
    });
    pane.cols = viewport.cols;
    pane.rows = viewport.rows;
    pane.session.refreshScreenText();
    fx.ptyResize(pane.pty_key, pane.cols, pane.rows);
}

pub fn remoteText(model: *Model, ref: TerminalRef, event: Event) void {
    if (event.text.len == 0) return;
    const state = model.remoteUi(ref) orelse return;
    if (state.search.open) {
        _ = remote_commands.input(model, ref, event.text);
        return;
    }
    if (state.selecting) return;
    const remote = model.phuxForOwner(state.owner) orelse return;
    remote_selection.clear(model, state);
    // A reconnecting terminal cannot scroll yet but still holds the text.
    remote.scrollViewport(state.owner, .{ .kind = .bottom }) catch {};
    remote.sendKey(state.owner, &.{
        .action = .press,
        .physical = @enumFromInt(0),
        .text = event.text,
        // The text is already committed by the host's layout/IME. Applying
        // Option or Shift again would turn composed text into another chord.
        .modifiers = .{},
    }) catch {};
}

pub fn modifiers(event: Event) contract.ModifierMask {
    return .{
        .shift = event.modifiers.shift,
        .control = event.modifiers.control,
        .alt = event.modifiers.alt,
        .super = event.modifiers.super and !event.modifiers.control,
    };
}

pub fn remoteKey(model: *Model, ref: TerminalRef, event: Event) void {
    const owner = model.terminalOwner(ref) orelse return;
    remoteKeyForOwner(model, owner, event);
}

fn remoteKeyForOwner(model: *Model, owner: contract.ReplicaOwner, event: Event) void {
    const remote = model.phuxForOwner(owner) orelse return;
    const input = runtime.providerKey(event) orelse return;
    remote.sendKey(owner, &input) catch {};
}

/// A release returns to the owner of its press, even after focus moved. A
/// reconnect or terminal replacement invalidates that owner rather than
/// delivering the old release to a new process.
pub fn releaseKey(model: *Model, fx: anytype, event: Event) void {
    const owner = switch (takeHeldKeyOwner(model, event.key)) {
        .owner => |value| value,
        else => return,
    };
    if (!terminalAcceptsKeys(model, owner.terminal_ref)) return;
    if (model.provider.terminal(owner.terminal_ref)) |pane| {
        runtime.encodeKeyEvent(pane, fx, event, .release);
        return;
    }
    remoteKeyForOwner(model, owner, event);
}

pub fn rememberKey(model: *Model, ref: TerminalRef, event: Event) void {
    if (!terminalAcceptsKeys(model, ref)) return;
    const owner = model.terminalOwner(ref) orelse return;
    rememberHeldKey(model, owner, event.key);
}

fn terminalAcceptsKeys(model: *Model, ref: TerminalRef) bool {
    if (model.provider.terminal(ref)) |pane| {
        return pane.acceptsInput() and !pane.selecting and !pane.session.search.open;
    }
    const state = model.remoteUi(ref) orelse return false;
    if (state.selecting or state.search.open) return false;
    return model.ownerIsCurrent(state.owner);
}

/// The Phux focus target: the focused pane, while the window has key and the
/// Web surface is not selected.
pub fn remoteFocusTarget(model: *const Model) ?TerminalRef {
    if (!model.focused) return null;
    if (model.wsConst().web_selected) return null;
    const terminal_ref = model.focusedTerminalRef() orelse return null;
    if (support.providerKind(terminal_ref) != .phux) return null;
    return terminal_ref;
}

/// Bracket-encode a clipboard result for this terminal and admit it as one
/// outbound payload, or refuse the whole paste.
pub fn pasteClipboardText(model: *Model, pane: *local.Pane, fx: anytype, text: []const u8) void {
    const fence_bytes = "\x1b[200~".len;
    const staging = pane.session.gpa.alloc(u8, text.len + fence_bytes * 2) catch {
        model.paste_failed = true;
        return;
    };
    defer pane.session.gpa.free(staging);

    const body = staging[fence_bytes .. fence_bytes + text.len];
    @memcpy(body, text);
    const parts = vt.input.encodePaste(body, .fromTerminal(&pane.session.term));
    const start = fence_bytes - parts[0].len;
    @memcpy(staging[start..fence_bytes], parts[0]);
    @memcpy(staging[fence_bytes + text.len .. fence_bytes + text.len + parts[2].len], parts[2]);
    const encoded = staging[start .. fence_bytes + text.len + parts[2].len];

    pane.session.scrollToBottom();
    // Retained terminal replies predate this input and must go first; if they
    // cannot move, refusing the paste is the only ordering-safe outcome.
    runtime.moveResponsesToOutbound(pane, fx);
    if (pane.session.response_len > 0 or encoded.len > pane.outbound_buffer.len) {
        pane.outbound_dropped += encoded.len;
        model.paste_failed = true;
        return;
    }
    if (!runtime.enqueueOutbound(pane, fx, encoded)) {
        pane.outbound_dropped += encoded.len;
        model.paste_failed = true;
    }
}

/// Keyboard selection on a Phux pane, expressed as provider anchors.
pub const remote_selection = struct {
    pub fn begin(model: *Model, state: *model_module.RemoteUiState) void {
        clear(model, state);
        const presentation = presentationForOwner(model, state.owner) orelse return;
        const cursor = presentation.grid.cursor orelse native_sdk.canvas.TerminalCursor{};
        const remote = model.phuxForOwner(state.owner) orelse return;
        const point: contract.DocumentPoint = .{ .space = .viewport, .row = @as(u32, cursor.y), .column = cursor.x };
        const start = remote.createAnchor(state.owner, point) catch return;
        const end = remote.createAnchor(state.owner, point) catch {
            remote.releaseAnchor(state.owner, start);
            return;
        };
        remote.setSelection(state.owner, start, end, false) catch {
            remote.releaseAnchor(state.owner, start);
            remote.releaseAnchor(state.owner, end);
            return;
        };
        state.selecting = true;
        state.rectangle = false;
        state.start_anchor = start.opaque_id;
        state.end_anchor = end.opaque_id;
        state.head_x = cursor.x;
        state.head_y = cursor.y;
    }

    pub fn apply(model: *Model, state: *model_module.RemoteUiState) void {
        const remote = model.phuxForOwner(state.owner) orelse return;
        const next = remote.createAnchor(state.owner, .{ .space = .viewport, .row = state.head_y, .column = state.head_x }) catch return;
        remote.setSelection(state.owner, .{ .opaque_id = state.start_anchor }, next, state.rectangle) catch {
            remote.releaseAnchor(state.owner, next);
            return;
        };
        if (state.end_anchor != 0 and state.end_anchor != state.start_anchor)
            remote.releaseAnchor(state.owner, .{ .opaque_id = state.end_anchor });
        state.end_anchor = next.opaque_id;
    }

    pub fn move(model: *Model, state: *model_module.RemoteUiState, dx: i32, dy: i32) void {
        const presentation = presentationForOwner(model, state.owner) orelse return;
        const max_x: i32 = @max(0, @as(i32, presentation.cols) - 1);
        const max_y: i64 = @max(0, @as(i64, presentation.rows) - 1);
        state.head_x = @intCast(std.math.clamp(@as(i32, state.head_x) + dx, 0, max_x));
        state.head_y = @intCast(std.math.clamp(@as(i64, state.head_y) + dy, 0, max_y));
        apply(model, state);
    }

    pub fn clear(model: *Model, state: *model_module.RemoteUiState) void {
        defer {
            state.selecting = false;
            state.rectangle = false;
            state.start_anchor = 0;
            state.end_anchor = 0;
            state.gesture_handle = 0;
        }
        const remote = model.phuxForOwner(state.owner) orelse return;
        remote.clearSelection(state.owner) catch {};
        if (state.start_anchor != 0)
            remote.releaseAnchor(state.owner, .{ .opaque_id = state.start_anchor });
        if (state.end_anchor != 0 and state.end_anchor != state.start_anchor)
            remote.releaseAnchor(state.owner, .{ .opaque_id = state.end_anchor });
    }
};

fn heldKeyFingerprint(key: []const u8) u64 {
    var fingerprint: u64 = 14695981039346656037;
    for (key) |byte| {
        fingerprint ^= std.ascii.toLower(byte);
        fingerprint *%= 1099511628211;
    }
    return if (fingerprint == 0) 1 else fingerprint;
}

fn rememberHeldKey(model: *Model, owner: contract.ReplicaOwner, key: []const u8) void {
    const fingerprint = heldKeyFingerprint(key);
    var target: usize = @intCast(fingerprint % model_module.max_held_terminal_keys);
    for (&model.held_terminal_keys, 0..) |*held, index| {
        if (held.fingerprint == fingerprint) {
            target = index;
            break;
        }
        if (held.fingerprint == 0) target = index;
    }
    model.held_terminal_keys[target] = .{ .fingerprint = fingerprint, .owner = owner };
}

/// `.consume` means the press belonged to an owner that no longer exists.
fn takeHeldKeyOwner(model: *Model, key: []const u8) union(enum) { none, consume, owner: contract.ReplicaOwner } {
    const fingerprint = heldKeyFingerprint(key);
    for (&model.held_terminal_keys) |*held| {
        if (held.fingerprint != fingerprint) continue;
        const owner = held.owner;
        held.* = .{};
        if (!model.ownerIsCurrent(owner)) return .consume;
        return .{ .owner = owner };
    }
    return .none;
}
