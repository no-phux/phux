//! Closing presentation never grants authority to kill coordinator-owned work.
const model_module = @import("model.zig");
const support = @import("phux_support.zig");
const local = @import("../providers/local/provider.zig");
const layout = @import("layout.zig");
const pointer = @import("pointer_input.zig");
const scene = @import("native/scene.zig");
const Model = model_module.Model;

pub fn closePane(model: *Model, fx: anytype, ref: support.TerminalRef, kill: bool) bool {
    const where = model.locateTerminal(ref) orelse return false;
    const workspace = model.wsAt(where.window) orelse return false;
    const tree = &workspace.tabs[where.tab];
    pointer.endCapturesForTerminal(model, fx, ref);
    cancelClipboard(model, fx, ref);
    if (!releaseRemote(model, ref)) return false;
    _ = tree.closeTerminal(ref) orelse return false;
    model.pruneAttachmentState();
    releaseLocal(model, fx, ref, kill);
    if (tree.isEmpty()) workspace.dropTab(where.tab);
    workspace.tab_limit_refused = false;
    pointer.endHiddenCaptures(model, fx);
    if (workspace.tab_count == 0) retireWindow(model, fx, where.window);
    return true;
}

/// Where an ended remote shell's pane sat, captured before the drain's
/// workspace snapshot is applied; `window` is null when it was already gone.
const EndedPane = struct {
    ref: support.TerminalRef,
    window: ?usize,
    epoch: u64,
};

/// The remote shells that ENDED in one provider drain. Cockpit's rule is that
/// a shell that ran and ended closes its pane at any status. They are taken
/// from the provider before that drain's workspace snapshot is projected, so
/// each pane's window is known whichever of the close and the snapshot
/// arrived first, and every ordering settles the window the same way.
pub const EndedPanes = struct {
    /// More than this in one drain stay queued on the provider for the next.
    const capacity = 64;
    entries: [capacity]EndedPane = undefined,
    count: usize = 0,

    pub fn take(self: *EndedPanes, model: *const Model, remote: anytype) void {
        if (comptime !support.phux_enabled) return;
        while (self.count < capacity) : (self.count += 1) {
            const ref = remote.takeEnded() orelse return;
            self.entries[self.count] = locateEnded(model, ref);
        }
    }

    /// Close each pane the snapshot has not already removed, then settle the
    /// window it emptied. A pane whose window was unknown (a snapshot removed
    /// it in an earlier drain) settles the windows that snapshot emptied.
    pub fn retire(self: *const EndedPanes, model: *Model, fx: anytype, emptied: *EmptiedWindows) bool {
        var changed = false;
        for (self.entries[0..self.count]) |entry| {
            changed = retireEnded(model, fx, entry, emptied) or changed;
        }
        return changed;
    }
};

fn locateEnded(model: *const Model, ref: support.TerminalRef) EndedPane {
    const where = model.locateTerminal(ref) orelse return .{ .ref = ref, .window = null, .epoch = 0 };
    return .{ .ref = ref, .window = where.window, .epoch = model.window_epochs[where.window] };
}

fn retireEnded(model: *Model, fx: anytype, entry: EndedPane, emptied: *EmptiedWindows) bool {
    const window = entry.window orelse return emptied.settleAll(model, fx);
    const closed = closeEndedPane(model, entry.ref);
    const settled = settleEmptiedWindow(model, fx, window, entry.epoch);
    emptied.forget(window);
    return closed or settled;
}

/// Close an ended remote shell's pane unless the snapshot already removed it.
/// A remote ref owns no pty or pointer capture, and a clipboard transfer
/// still in flight for it is fenced by its owner when it lands.
fn closeEndedPane(model: *Model, ref: support.TerminalRef) bool {
    const where = model.locateTerminal(ref) orelse return false;
    const workspace = model.wsAt(where.window) orelse return false;
    if (!releaseRemote(model, ref)) return false;
    const tree = &workspace.tabs[where.tab];
    _ = tree.closeTerminal(ref) orelse return false;
    model.pruneAttachmentState();
    if (tree.isEmpty()) workspace.dropTab(where.tab);
    workspace.tab_limit_refused = false;
    return true;
}

/// A window an ended shell left without tabs. A keep-empty session keeps it,
/// showing Empty session (ADR-0114). Otherwise the emptied window closes, and
/// closing the last window quits (DECISIONS.md, "An emptied window closes").
fn settleEmptiedWindow(model: *Model, fx: anytype, window: usize, epoch: u64) bool {
    if (!model.windowOpen(window) or model.window_epochs[window] != epoch) return false;
    const workspace = model.wsAt(window) orelse return false;
    if (workspace.tab_count != 0) return false;
    if (keepsEmptySession(model, window)) {
        workspace.web_selected = false;
        return true;
    }
    retireWindow(model, fx, window);
    return true;
}

fn keepsEmptySession(model: *const Model, window: usize) bool {
    const remote = model.phuxForWindowConst(window) orelse return false;
    const id = remote.selectedSessionId() orelse return false;
    for (remote.sessionCatalog()) |session| {
        if (session.id == id) return session.keep_empty;
    }
    return false;
}

/// Windows a workspace snapshot emptied, remembered until an ended shell
/// explains them or they hold tabs again. This covers the snapshot-first
/// ordering: the pane was gone before its EXITED arrived, so only the
/// snapshot's effect on its window says where it was.
pub const EmptiedWindows = struct {
    epochs: [model_module.max_windows]?u64 = @splat(null),

    pub fn occupied(model: *const Model) [model_module.max_windows]bool {
        var result: [model_module.max_windows]bool = @splat(false);
        for (&result, 0..) |*held, window| {
            const workspace = model.wsAtConst(window) orelse continue;
            held.* = workspace.tab_count != 0;
        }
        return result;
    }

    /// Record the windows that held tabs before a drain and none after it.
    pub fn observe(self: *EmptiedWindows, model: *const Model, before: [model_module.max_windows]bool) void {
        for (before, 0..) |held, window| self.epochs[window] = emptiedEpoch(model, window, held, self.epochs[window]);
    }

    fn settleAll(self: *EmptiedWindows, model: *Model, fx: anytype) bool {
        var changed = false;
        for (&self.epochs, 0..) |*slot, window| {
            const epoch = slot.* orelse continue;
            slot.* = null;
            changed = settleEmptiedWindow(model, fx, window, epoch) or changed;
        }
        return changed;
    }

    fn forget(self: *EmptiedWindows, window: usize) void {
        self.epochs[window] = null;
    }
};

fn emptiedEpoch(model: *const Model, window: usize, held: bool, current: ?u64) ?u64 {
    const workspace = model.wsAtConst(window) orelse return null;
    if (workspace.tab_count != 0) return null;
    return if (held) model.window_epochs[window] else current;
}

fn releaseRemote(model: *Model, ref: support.TerminalRef) bool {
    if (comptime !support.phux_enabled) return true;
    if (support.providerKind(ref) != .phux) return true;
    const remote = model.phuxForRef(ref) orelse return true;
    if (!remote.contains(ref)) return true;
    if (remote.state() != .attached) return true;
    // An ended shell has nothing left to detach from.
    if (remote.phase(ref) == .ended) return true;
    _ = remote.requestDetach(ref) catch {
        model.terminal_limit_refused = true;
        return false;
    };
    model.terminal_limit_refused = false;
    return true;
}

fn cancelClipboard(model: *Model, fx: anytype, ref: support.TerminalRef) void {
    if (model.copy_inflight and model.copy_owner.terminal_ref.eql(ref)) fx.cancel(local.clipboard_key);
    if (model.paste_inflight and model.paste_owner.terminal_ref.eql(ref)) fx.cancel(local.paste_clipboard_key);
}

fn releaseLocal(model: *Model, fx: anytype, ref: support.TerminalRef, kill: bool) void {
    const pane = model.provider.terminal(ref) orelse return;
    const live = pane.phase == .starting or pane.phase == .live;
    const key = pane.pty_key;
    _ = model.provider.destroyTerminal(ref);
    if (kill and live) fx.ptyKill(key);
    model.terminal_limit_refused = false;
}

pub fn closeTab(model: *Model, fx: anytype, window: usize, index: usize) bool {
    const workspace = model.wsAt(window) orelse return false;
    const tree = workspace.treeConst(index) orelse return false;
    var refs: [layout.max_panes]support.TerminalRef = undefined;
    const count = tree.terminals(&refs);
    var changed = false;
    for (refs[0..count]) |ref| changed = closePane(model, fx, ref, true) or changed;
    return changed;
}

pub fn closeWindow(model: *Model, fx: anytype, index: usize) bool {
    const workspace = model.wsAt(index) orelse return false;
    var remaining = workspace.tab_count;
    var changed = false;
    // Each original tab is visited once. A locally refused detach retains its
    // placement; it must never spin the owning thread waiting for readiness.
    while (remaining != 0) {
        remaining -= 1;
        changed = closeTab(model, fx, index, remaining) or changed;
    }
    const current = model.wsAt(index) orelse return changed;
    if (current.tab_count != 0) return changed;
    retireWindow(model, fx, index);
    return true;
}

fn retireWindow(model: *Model, fx: anytype, index: usize) void {
    model.closeWindow(index);
    model.window_limit_refused = false;
    // Secondary windows retire through the declarative window list. Main is
    // the manifest's fixed window and needs one explicit platform close.
    if (index == 0) fx.closeWindow(scene.main_window_label);
    if (model.primary_open) return;
    for (model.secondary) |slot| if (slot != null) return;
    fx.quitApp();
}
