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
    _ = tree.closeTerminal(ref) orelse return false;
    model.pruneAttachmentState();
    releaseLocal(model, fx, ref, kill);
    if (tree.isEmpty()) workspace.dropTab(where.tab);
    workspace.tab_limit_refused = false;
    pointer.endHiddenCaptures(model, fx);
    if (workspace.tab_count == 0) retireWindow(model, fx, where.window);
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
    for (refs[0..count]) |ref| _ = closePane(model, fx, ref, true);
    return count != 0;
}

pub fn closeWindow(model: *Model, fx: anytype, index: usize) bool {
    if (!model.windowOpen(index)) return false;
    while (model.wsAt(index)) |workspace| {
        if (workspace.tab_count == 0) break;
        if (!closeTab(model, fx, index, 0)) break;
    }
    if (model.windowOpen(index)) retireWindow(model, fx, index);
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
