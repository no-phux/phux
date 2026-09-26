//! Provider-qualified interaction shared by native event entry points.
//! Clipboard completions belong to the replica that requested them, not focus.
const std = @import("std");
const native_sdk = @import("native_sdk");
const contract = @import("provider_contract");
const model_module = @import("model.zig");
const local = @import("../providers/local/provider.zig");
const runtime = @import("terminal_runtime.zig");
const update = @import("update.zig");
const remote_commands = @import("native/remote_presentation_commands.zig");

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
    const remote = model.phuxForOwner(model.paste_owner) orelse return;
    if (model.paste_target == .search_needle) {
        pasteRemoteSearch(model, text);
        return;
    }
    remote.sendPaste(model.paste_owner, text, false) catch {
        model.paste_failed = true;
    };
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
    update.pasteClipboardText(model, pane, fx, text);
}

pub fn resize(model: *Model, fx: anytype, ref: TerminalRef, viewport: contract.Viewport) void {
    if (model.provider.terminal(ref)) |pane| {
        resizeLocal(pane, fx, viewport);
        return;
    }
    // Each coordinator's terminal is sized on its own server, only once a
    // pane shows it and it is live.
    const owner = model.terminalOwner(ref) orelse return;
    const remote = model.phuxForOwner(owner) orelse return;
    if (remote.lastViewport(ref)) |last| if (last.eql(viewport)) return;
    const presentation = presentationForOwner(model, owner) orelse return;
    if (presentation.phase != .live) return;
    remote.viewportResize(ref, viewport) catch {};
}

fn resizeLocal(pane: *local.Pane, fx: anytype, viewport: contract.Viewport) void {
    if (pane.cols == viewport.cols and pane.rows == viewport.rows) return;
    if (!pane.session.resize(viewport.cols, viewport.rows)) return;
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
    update.remote_selection.clear(model, state);
    remote.scrollViewport(state.owner, .{ .kind = .bottom }) catch return;
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
    const owner = switch (update.key_owners.take(model, event.key)) {
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
    update.key_owners.remember(model, owner, event.key);
}

fn terminalAcceptsKeys(model: *Model, ref: TerminalRef) bool {
    if (model.provider.terminal(ref)) |pane| {
        return pane.acceptsInput() and !pane.selecting and !pane.session.search.open;
    }
    const state = model.remoteUi(ref) orelse return false;
    if (state.selecting or state.search.open) return false;
    return model.ownerIsCurrent(state.owner);
}
