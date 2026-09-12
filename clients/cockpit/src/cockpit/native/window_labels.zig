//! Presentation text for the native window catalog; authority lives elsewhere.
const std = @import("std");
const windows = @import("ts_window_navigation.zig");
const projection = @import("workspace_projection.zig");
const Model = @import("../model.zig").Model;
const Workspace = @import("../model.zig").Workspace;
const support = @import("../phux_support.zig");
const layout = @import("../layout.zig");

pub fn encode(model: *const Model, revision: u64, request: []const u8, out: []u8) windows.Error![]const u8 {
    return windows.encodeWithLabels(model, revision, request, out, Labels{});
}

pub const Labels = struct {
    /// Search underlying text independently of byte-bounded wire presentation.
    pub fn matches(self: Labels, model: *const Model, target: windows.Target, query: []const u8) bool {
        const selected = target.resolve(model) orelse return false;
        if (query.len == 0) return true;
        if (self.displayMatches(model, target, query)) return true;
        const ws = model.wsAtConst(target.window).?;
        if (ws.tab_count == 0) {
            return emptyMatches(model, target.window, query);
        }
        if (selected.tab) |tab| return tabMatches(model, ws, tab, query);
        for (0..ws.tab_count) |tab| if (tabMatches(model, ws, tab, query)) return true;
        return false;
    }

    fn displayMatches(self: Labels, model: *const Model, target: windows.Target, query: []const u8) bool {
        var buffer: [240]u8 = undefined;
        if (windows.contains(self.label(model, target, &buffer), query)) return true;
        return windows.contains(self.detail(model, target, &buffer), query);
    }

    pub fn label(_: Labels, model: *const Model, target: windows.Target, out: []u8) []const u8 {
        const selected = target.resolve(model) orelse return "Unavailable window";
        const ws = model.wsAtConst(target.window).?;
        const tab = selected.tab orelse {
            return std.fmt.bufPrint(out, "Window {d}", .{target.window + 1}) catch "Window";
        };
        var full: [1024]u8 = undefined;
        return windows.display(tabTitle(model, ws, tab, &full), out);
    }

    pub fn detail(_: Labels, model: *const Model, target: windows.Target, out: []u8) []const u8 {
        const ws = model.wsAtConst(target.window) orelse return "Window unavailable";
        if (ws.tab_count == 0) return emptyDetail(model, target.window, out);
        const selected = target.resolve(model) orelse return "Window unavailable";
        const tab = selected.tab orelse ws.selected_tab;
        var full: [1024]u8 = undefined;
        var bounded: [48]u8 = undefined;
        var context_buffer: [70]u8 = undefined;
        const title = windows.display(tabTitle(model, ws, tab, &full), &bounded);
        const context = tabContext(model, ws, tab, &context_buffer);
        return std.fmt.bufPrint(out, "Window {d} · Tab {d} · {s} · {s}", .{ target.window + 1, tab + 1, context, title }) catch "Open window";
    }
};

fn emptyMatches(model: *const Model, window: usize, query: []const u8) bool {
    const view = @import("empty_session.zig").view(model, window) orelse return false;
    return windows.contains(view.name, query) or windows.contains(view.host, query);
}

fn emptyDetail(model: *const Model, window: usize, out: []u8) []const u8 {
    const view = @import("empty_session.zig").view(model, window) orelse return "Empty window · New Tab";
    var name: [64]u8 = undefined;
    var host: [48]u8 = undefined;
    return std.fmt.bufPrint(out, "Empty session · {s} · {s}", .{ windows.display(view.name, &name), windows.display(view.host, &host) }) catch "Empty session · New Tab";
}

fn tabContext(model: *const Model, ws: *const Workspace, tab: usize, out: []u8) []const u8 {
    const ref = ws.tabTerminal(tab) orelse return "No focused terminal";
    if (ref.provider_id == .local) return "Local PTY";
    if (comptime !@import("../phux_support.zig").phux_enabled) return "Phux session";
    const remote = providerForTab(model, ws, tab) orelse return "Machine unavailable";
    const id = remote.terminalSession(ref) orelse return "Session unavailable";
    return sessionContext(remote, id, out);
}

fn providerForTab(model: *const Model, ws: *const Workspace, tab: usize) ?*const support.PhuxProvider {
    if (comptime !support.phux_enabled) return null;
    // Independent attachment models resolve the tree's exact context. A failed
    // lookup there must never fall back to an ambient same-machine connection.
    if (comptime @hasDecl(Model, "phuxForTreeConst")) return model.phuxForTreeConst(ws.treeConst(tab) orelse return null);
    return model.phuxForRefConst(ws.tabTerminal(tab) orelse return null);
}

fn sharedTitle(remote: anytype, id: ?[16]u8) ?[]const u8 {
    const shared = id orelse return null;
    for (remote.workspaceSnapshot().windows) |*window| {
        if (!std.mem.eql(u8, &window.id, &shared)) continue;
        return if (window.name.len == 0) null else window.name.slice();
    }
    return null;
}

fn tabTitle(model: *const Model, ws: *const Workspace, tab: usize, out: []u8) []const u8 {
    const ref = ws.tabTerminal(tab) orelse return "Terminal";
    if (ref.provider_id == .local) return projection.terminalTitleInto(model, ref, out);
    if (comptime !support.phux_enabled) return "Phux";
    const remote = providerForTab(model, ws, tab) orelse return "Machine unavailable";
    if (sharedTitle(remote, ws.shared_ids[tab])) |name| return name;
    for (remote.catalogTerminals()) |*entry| {
        if (!entry.terminal_ref.eql(ref)) continue;
        if (entry.title.len != 0) return entry.title.slice();
        if (entry.cwd.len != 0) return entry.cwd.slice();
    }
    return "Phux";
}

fn tabMatches(model: *const Model, ws: *const Workspace, tab: usize, query: []const u8) bool {
    const tree = ws.treeConst(tab) orelse return false;
    var refs: [layout.max_panes]layout.TerminalRef = undefined;
    const count = tree.terminals(&refs);
    for (refs[0..count]) |ref| {
        if (terminalMatches(model, ws, tab, ref, query)) return true;
    }
    return false;
}

fn terminalMatches(model: *const Model, ws: *const Workspace, tab: usize, ref: layout.TerminalRef, query: []const u8) bool {
    if (ref.provider_id == .local) {
        const pane = model.provider.terminalConst(ref) orelse return false;
        return windows.contains(pane.title(), query) or windows.contains(pane.pwd(), query);
    }
    if (comptime !support.phux_enabled) return false;
    const remote = providerForTab(model, ws, tab) orelse return false;
    if (windows.contains(remote.remoteLabel() orelse "This Mac", query)) return true;
    if (sharedTitle(remote, ws.shared_ids[tab])) |name| if (windows.contains(name, query)) return true;
    return remoteTerminalMatches(remote, ref, query);
}

fn remoteTerminalMatches(remote: anytype, ref: layout.TerminalRef, query: []const u8) bool {
    for (remote.catalogTerminals()) |*entry| {
        if (!entry.terminal_ref.eql(ref)) continue;
        if (windows.contains(entry.title.slice(), query) or windows.contains(entry.cwd.slice(), query)) return true;
    }
    const id = remote.terminalSession(ref) orelse return false;
    for (remote.sessionCatalog()) |session| {
        if (session.id == id and windows.contains(session.name, query)) return true;
    }
    return false;
}

fn sessionContext(remote: anytype, id: u32, out: []u8) []const u8 {
    var name: [34]u8 = undefined;
    var host: [28]u8 = undefined;
    for (remote.sessionCatalog()) |session| {
        if (session.id != id) continue;
        return std.fmt.bufPrint(out, "{s} · {s}", .{ windows.display(session.name, &name), windows.display(remote.remoteLabel() orelse "This Mac", &host) }) catch "Phux session";
    }
    return std.fmt.bufPrint(out, "Session #{d}", .{id}) catch "Phux session";
}
