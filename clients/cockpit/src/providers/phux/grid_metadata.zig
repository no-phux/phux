//! Read-only companion metadata for one borrowed v1 C grid. This is not a
//! second render pass: the query reads the producer's same-pass cache.
const std = @import("std");
const canvas = @import("native_sdk").canvas;
const c = @import("abi.zig").c;

const default_tokens: canvas.DesignTokens = .{};

/// Renderer policy, not terminal state. Local Palette.resolveFgRaw currently
/// enables ANSI-8 bold-as-bright; cursor/selection fallbacks are theme tokens.
pub const Policy = struct {
    bold_as_bright: bool = true,
    foreground: canvas.Color = default_tokens.colors.text,
    background: canvas.Color = default_tokens.colors.background,
    cursor_fallback: canvas.Color = default_tokens.colors.accent,
    selection_color: canvas.Color = default_tokens.colors.accent,
};

pub fn foreground(metadata: *const c.PhuxTerminalGridMetadata, policy: Policy) canvas.Color {
    if (metadata.has_foreground) return rgb(metadata.foreground);
    return if (metadata.reverse_colors) policy.background else policy.foreground;
}

pub fn background(metadata: *const c.PhuxTerminalGridMetadata, policy: Policy) canvas.Color {
    if (metadata.has_background) return rgb(metadata.background);
    return if (metadata.reverse_colors) policy.foreground else policy.background;
}

pub fn read(client: *const c.PhuxClient, view: *const c.PhuxTerminalGridView) !c.PhuxTerminalGridMetadata {
    var metadata = std.mem.zeroes(c.PhuxTerminalGridMetadata);
    metadata.size = @sizeOf(c.PhuxTerminalGridMetadata);
    metadata.version = c.PHUX_CLIENT_ABI_VERSION;
    if (c.phux_client_terminal_grid_metadata(client, &view.terminal_id, &metadata) != c.PHUX_CLIENT_OK)
        return error.Protocol;
    return metadata;
}

pub fn validate(view: *const c.PhuxTerminalGridView, metadata: *const c.PhuxTerminalGridMetadata) !void {
    if (metadata.size < @sizeOf(c.PhuxTerminalGridMetadata) or metadata.version != c.PHUX_CLIENT_ABI_VERSION)
        return error.Protocol;
    if (!sameGeneration(view, metadata)) return error.Protocol;
    try validateShape(view, metadata);
    try validateCursor(view, metadata);
    for (0..metadata.cell_count) |index| {
        if (metadata.cells[index].foreground_kind > c.PHUX_GRID_COLOR_RGB) return error.Protocol;
    }
}

fn validateShape(view: *const c.PhuxTerminalGridView, metadata: *const c.PhuxTerminalGridMetadata) !void {
    if (metadata.cols != view.cols or metadata.rows != view.rows or metadata.cell_count != view.cell_count)
        return error.Protocol;
    if (metadata.cell_count != 0 and metadata.cells == null) return error.Protocol;
}

fn sameGeneration(view: *const c.PhuxTerminalGridView, metadata: *const c.PhuxTerminalGridMetadata) bool {
    return metadata.stream_id == view.stream_id and metadata.bootstrap_id == view.bootstrap_id and
        metadata.last_seq == view.last_seq and metadata.document_revision == view.document_revision;
}

fn validateCursor(view: *const c.PhuxTerminalGridView, metadata: *const c.PhuxTerminalGridMetadata) !void {
    if (!view.cursor_visible) return;
    if (view.cursor_col >= view.cols or view.cursor_row >= view.rows) return error.Protocol;
    if (metadata.cursor_at_wide_tail and view.cursor_col == 0) return error.Protocol;
}

pub fn rgb(value: c.PhuxGridRgb) canvas.Color {
    return canvas.Color.rgb8(value.r, value.g, value.b);
}
