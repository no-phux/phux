//! The retained search field shared by local and remote terminal panes.
//! Uses the original search band's tokens and block caret (view.zig before
//! the TypeScript chrome migration). Keyboard ownership stays in the engine.
const std = @import("std");
const native_sdk = @import("native_sdk");
const model_module = @import("../model.zig");
const contract = @import("provider_contract");
const layout = @import("../layout.zig");
const projection = @import("workspace_projection.zig");
const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;

pub const command_reserve: usize = 4;
pub const View = struct { needle: []const u8, count: usize, ordinal: usize, failed: bool = false };

pub fn paintWorkspace(model: *const model_module.Model, builder: *canvas.Builder, workspace: *const model_module.Workspace, size: geometry.SizeF, tokens: canvas.DesignTokens, focused: bool) !void {
    const ref = projection.workspaceTerminalRef(model, workspace) orelse return;
    const tree = workspace.selectedTreeConst() orelse return;
    const value = viewIn(model, tree, ref) orelse return;
    const rect = projection.workspaceChromeIn(model, workspace, size).search;
    try paint(value, builder, rect, tokens, 0, focused);
}

pub fn view(model: *const model_module.Model, ref: contract.TerminalRef) ?View {
    return viewForState(model, ref, model.remoteUiConst(ref));
}

pub fn viewIn(model: *const model_module.Model, tree: *const layout.Tree, ref: contract.TerminalRef) ?View {
    const presentation = model.remotePaintPresentationIn(tree, ref);
    const state = if (presentation) |value| model.remoteUiForOwnerConst(value.owner) else null;
    return viewForState(model, ref, state);
}

fn viewForState(model: *const model_module.Model, ref: contract.TerminalRef, remote_state: ?*const model_module.RemoteUiState) ?View {
    if (model.provider.terminalConst(ref)) |pane| {
        if (!pane.session.search.open) return null;
        return .{ .needle = pane.session.searchNeedle(), .count = pane.session.searchMatchCount(), .ordinal = pane.session.searchMatchOrdinal() };
    }
    const state = remote_state orelse return null;
    if (!state.search.open) return null;
    return .{ .needle = state.search.needle(), .count = state.search.count, .ordinal = state.search.index + 1, .failed = state.search.failed };
}

fn statusText(value: View, buf: []u8) []const u8 {
    if (value.failed) return "Search unavailable";
    if (value.needle.len == 0) return "";
    if (value.count == 0) return "No matches";
    return std.fmt.bufPrint(buf, "{d} of {d}", .{ value.ordinal, value.count }) catch "";
}

pub fn paint(value: View, builder: *canvas.Builder, rect: geometry.RectF, tokens: canvas.DesignTokens, index: usize, focused: bool) !void {
    const inset = projection.chrome_band_inset;
    const frame = geometry.RectF.init(rect.x, rect.y, rect.width, @min(rect.height, projection.search_bar_height));
    if (frame.width <= inset * 2 or frame.height <= 0) return;
    const id: u64 = 0x0d00 + index * command_reserve;
    try builder.fillRect(.{ .id = id, .rect = frame, .fill = .{ .color = tokens.colors.surface_subtle } });
    var buf: [80]u8 = undefined;
    const status = statusText(value, &buf);
    const size = tokens.typography.label_size;
    const status_width = @min(frame.width - inset * 2, canvas.measureTextWidthForFont(tokens.text_measure, tokens.typography.font_id, status, size));
    const width = @max(0, frame.width - inset * 3 - status_width - 8);
    const text = if (value.needle.len == 0) "Search scrollback" else value.needle;
    const baseline = frame.y + (frame.height + size * 0.7) / 2;
    try builder.drawText(.{
        .id = id + 1,
        .font_id = tokens.typography.font_id,
        .size = size,
        .origin = .{ .x = frame.x + inset, .y = baseline },
        .color = if (value.needle.len == 0) tokens.colors.text_muted else tokens.colors.text,
        .text = text,
        .text_layout = .{ .max_width = width, .wrap = .none, .overflow = .clip, .measure = tokens.text_measure },
    });
    try builder.drawText(.{
        .id = id + 2,
        .font_id = tokens.typography.font_id,
        .size = size,
        .origin = .{ .x = frame.x + frame.width - inset - status_width, .y = baseline },
        .color = if (value.failed or (value.needle.len != 0 and value.count == 0)) tokens.colors.warning else tokens.colors.text_muted,
        .text = try builder.allocTextBytes(status),
        .text_layout = .{ .max_width = status_width, .wrap = .none, .overflow = .clip, .measure = tokens.text_measure },
    });
    if (!focused) return;
    // Original field_caret_width/height: an 8x16 block remains visible at 1x.
    const caret_x = @min(width, canvas.measureTextWidthForFont(tokens.text_measure, tokens.typography.font_id, text, size));
    const caret_height = @min(frame.height, 16);
    try builder.fillRect(.{
        .id = id + 3,
        .rect = geometry.RectF.init(frame.x + inset + caret_x, frame.y + (frame.height - caret_height) / 2, @min(8, frame.width - inset * 2), caret_height),
        .fill = .{ .color = tokens.colors.accent },
    });
}
