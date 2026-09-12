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
        if (ws.tab_count == 0) return "Empty window · New Tab";
        const selected = target.resolve(model) orelse return "Window unavailable";
        const tab = selected.tab orelse ws.selected_tab;
        var full: [1024]u8 = undefined;
        var bounded: [100]u8 = undefined;
        const title = windows.display(projection.tabTitleInto(model, ws, tab, &full), &bounded);
        return std.fmt.bufPrint(out, "Window {d} · Tab {d} · {s}", .{ target.window + 1, tab + 1, title }) catch "Open window";
    }
};
