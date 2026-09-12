//! Presentation text for the native window catalog; authority lives elsewhere.
const std = @import("std");
const windows = @import("ts_window_navigation.zig");
const projection = @import("workspace_projection.zig");
const Model = @import("../model.zig").Model;

pub fn encode(model: *const Model, revision: u64, request: []const u8, out: []u8) windows.Error![]const u8 {
    return windows.encodeWithLabels(model, revision, request, out, Labels{});
}

pub const Labels = struct {
    pub fn label(_: Labels, model: *const Model, target: windows.Target, out: []u8) []const u8 {
        const selected = target.resolve(model) orelse return "Unavailable window";
        const ws = model.wsAtConst(target.window).?;
        const tab = selected.tab orelse {
            return std.fmt.bufPrint(out, "Window {d}", .{target.window + 1}) catch "Window";
        };
        var full: [1024]u8 = undefined;
        return windows.display(projection.tabTitleInto(model, ws, tab, &full), out);
    }

    pub fn detail(_: Labels, model: *const Model, target: windows.Target, out: []u8) []const u8 {
        const ws = model.wsAtConst(target.window) orelse return "Window unavailable";
        if (ws.tab_count == 0) return emptyDetail(model, target.window, out);
        const selected = target.resolve(model) orelse return "Window unavailable";
        const tab = selected.tab orelse ws.selected_tab;
        var full: [1024]u8 = undefined;
        var bounded: [48]u8 = undefined;
        var context_buffer: [70]u8 = undefined;
        const title = windows.display(projection.tabTitleInto(model, ws, tab, &full), &bounded);
        const context = tabContext(model, ws.tabTerminal(tab), &context_buffer);
        return std.fmt.bufPrint(out, "Window {d} · Tab {d} · {s} · {s}", .{ target.window + 1, tab + 1, context, title }) catch "Open window";
    }
};

fn emptyDetail(model: *const Model, window: usize, out: []u8) []const u8 {
    const view = @import("empty_session.zig").view(model, window) orelse return "Empty window · New Tab";
    var name: [64]u8 = undefined;
    var host: [48]u8 = undefined;
    return std.fmt.bufPrint(out, "Empty session · {s} · {s}", .{ windows.display(view.name, &name), windows.display(view.host, &host) }) catch "Empty session · New Tab";
}

fn tabContext(model: *const Model, terminal: ?@import("../model.zig").TerminalRef, out: []u8) []const u8 {
    const ref = terminal orelse return "No focused terminal";
    if (ref.provider_id == .local) return "Local PTY";
    if (comptime !@import("../phux_support.zig").phux_enabled) return "Phux session";
    const remote = model.phuxForRefConst(ref) orelse return "Machine unavailable";
    const id = remote.terminalSession(ref) orelse return "Session unavailable";
    return sessionContext(remote, id, out);
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
