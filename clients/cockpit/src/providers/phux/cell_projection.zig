//! C ABI cell interpretation. Colors are palette-resolved by the FFI, while
//! inverse/faint/hidden still need producer-side resolution for the SDK.
const std = @import("std");
const canvas = @import("native_sdk").canvas;
const c = @import("abi.zig").c;
const grid_metadata = @import("grid_metadata.zig");

pub const CellMetadata = struct {
    grid: *const c.PhuxTerminalGridMetadata,
    cell: c.PhuxGridCellMetadata,
    policy: grid_metadata.Policy,
};

pub const Span = struct {
    start: usize = 0,
    len: usize = 0,

    pub fn slice(span: Span, arena: []const u8) []const u8 {
        return arena[span.start..][0..span.len];
    }

    fn validate(span: Span, arena: []const u8) !void {
        if (span.start > arena.len or span.len > arena.len - span.start) return error.Protocol;
        _ = std.unicode.Utf8View.init(span.slice(arena)) catch return error.Protocol;
    }
};

pub const Source = struct {
    cells: []const c.PhuxTerminalCell,
    utf8: []const u8,
    hyperlink_bytes: usize,
};

pub fn text(raw: c.PhuxTerminalCell) Span {
    return .{ .start = raw.utf8_offset, .len = raw.utf8_len };
}

pub fn hyperlink(raw: c.PhuxTerminalCell) Span {
    if (raw.flags & c.PHUX_CLIENT_CELL_HYPERLINK == 0) return .{};
    return .{ .start = raw.hyperlink_offset, .len = raw.hyperlink_len };
}

fn validateShape(view: *const c.PhuxTerminalGridView) !void {
    if (view.cols > canvas.max_terminal_cols or view.rows > canvas.max_terminal_rows) return error.Protocol;
    if (view.cell_count != @as(usize, view.cols) * view.rows) return error.Protocol;
    if (view.cell_count != 0 and view.cells == null) return error.Protocol;
    if (view.utf8.len != 0 and view.utf8.data == null) return error.Protocol;
}

pub fn validate(view: *const c.PhuxTerminalGridView, budget: usize) !Source {
    try validateShape(view);
    const cells: []const c.PhuxTerminalCell = if (view.cell_count == 0) &.{} else view.cells[0..view.cell_count];
    const utf8: []const u8 = if (view.utf8.len == 0) &.{} else view.utf8.data[0..view.utf8.len];
    var text_bytes: usize = 0;
    var hyperlink_bytes: usize = 0;
    for (cells) |raw| {
        try admit(text(raw), utf8, &text_bytes, budget);
        try admit(hyperlink(raw), utf8, &hyperlink_bytes, budget);
    }
    return .{ .cells = cells, .utf8 = utf8, .hyperlink_bytes = hyperlink_bytes };
}

fn admit(span: Span, arena: []const u8, used: *usize, budget: usize) !void {
    if (span.len > budget - used.*) return error.Protocol;
    try span.validate(arena);
    used.* += span.len;
}

pub fn cell(raw: c.PhuxTerminalCell, cluster: []const u8) canvas.TerminalCell {
    return project(raw, cluster, null);
}

pub fn project(raw: c.PhuxTerminalCell, cluster: []const u8, metadata: ?CellMetadata) canvas.TerminalCell {
    const cp = codepoint(raw, cluster);
    var result: canvas.TerminalCell = .{
        .cp = cp,
        .cluster = if (cp == 0 or canvas.terminal_box.isBoxDrawing(cp)) "" else cluster,
        .fg = canvas.Color.rgb8(raw.foreground_r, raw.foreground_g, raw.foreground_b),
        .bg = canvas.Color.rgb8(raw.background_r, raw.background_g, raw.background_b),
        .wide = width(raw.wide),
        .bold = raw.flags & c.PHUX_CLIENT_CELL_BOLD != 0,
        .italic = raw.flags & c.PHUX_CLIENT_CELL_ITALIC != 0,
        .strikethrough = raw.flags & c.PHUX_CLIENT_CELL_STRIKETHROUGH != 0,
        .overline = raw.flags & c.PHUX_CLIENT_CELL_OVERLINE != 0,
        .underline = raw.underline != c.PHUX_UNDERLINE_NONE,
        .underline_style = underline(raw.underline),
        // FFI resolves SGR 59 to the raw foreground. It does not retain
        // explicit/default color provenance, so preserve its exact RGB.
        .underline_color = canvas.Color.rgb8(raw.underline_r, raw.underline_g, raw.underline_b),
    };
    if (metadata) |source| {
        result.fg = foreground(raw, source);
        if (source.cell.background_color_is_default) result.bg = grid_metadata.background(source.grid, source.policy);
        if (source.cell.underline_color_is_default) result.underline_color = null;
    }
    const default_background = if (metadata) |source| grid_metadata.background(source.grid, source.policy) else result.bg.?;
    resolveColors(raw.flags, &result, default_background);
    return result;
}

fn foreground(raw: c.PhuxTerminalCell, metadata: CellMetadata) canvas.Color {
    if (metadata.cell.foreground_kind == c.PHUX_GRID_COLOR_DEFAULT)
        return grid_metadata.foreground(metadata.grid, metadata.policy);
    const original = canvas.Color.rgb8(raw.foreground_r, raw.foreground_g, raw.foreground_b);
    if (!metadata.policy.bold_as_bright or raw.flags & c.PHUX_CLIENT_CELL_BOLD == 0) return original;
    if (metadata.cell.foreground_kind != c.PHUX_GRID_COLOR_PALETTE) return original;
    const index = metadata.cell.foreground_palette_index;
    // Local Palette.resolveFgRaw uses the ANSI-8 -> bright-8 mapping. The
    // index and live palette come from libghostty, not RGB matching.
    if (index >= 8) return original;
    return grid_metadata.rgb(metadata.grid.palette[@as(usize, index) + 8]);
}

fn resolveColors(flags: u32, result: *canvas.TerminalCell, default_background: canvas.Color) void {
    // push_flattened_cell in phux-client-ffi does NOT apply inverse. The
    // SDK consumes final colors and has no inverse flag: swap exactly once.
    if (flags & c.PHUX_CLIENT_CELL_INVERSE != 0) {
        const fg = result.fg;
        result.fg = result.bg.?;
        result.bg = fg;
        return;
    }
    if (flags & c.PHUX_CLIENT_CELL_FAINT == 0) return;
    // Match local Palette.resolveFg's half blend toward the terminal default,
    // after bold-as-bright and with inverse taking precedence over faint.
    const bg = default_background;
    result.fg = canvas.Color.rgba(
        result.fg.r + (bg.r - result.fg.r) * 0.5,
        result.fg.g + (bg.g - result.fg.g) * 0.5,
        result.fg.b + (bg.b - result.fg.b) * 0.5,
        1,
    );
}

fn codepoint(raw: c.PhuxTerminalCell, cluster: []const u8) u21 {
    if (raw.flags & c.PHUX_CLIENT_CELL_INVISIBLE != 0 or cluster.len == 0) return 0;
    // validate() checked the full cluster before any owned state was changed.
    const len = std.unicode.utf8ByteSequenceLength(cluster[0]) catch unreachable;
    return std.unicode.utf8Decode(cluster[0..len]) catch unreachable;
}

fn width(raw: u8) canvas.TerminalWide {
    return switch (raw) {
        c.PHUX_CELL_WIDE => .wide,
        c.PHUX_CELL_SPACER_TAIL, c.PHUX_CELL_SPACER_HEAD => .spacer,
        else => .narrow,
    };
}

fn underline(raw: u8) canvas.terminal_grid.TerminalUnderline {
    return switch (raw) {
        c.PHUX_UNDERLINE_NONE, c.PHUX_UNDERLINE_SINGLE => .single,
        c.PHUX_UNDERLINE_DOUBLE => .double,
        c.PHUX_UNDERLINE_CURLY => .curly,
        c.PHUX_UNDERLINE_DOTTED => .dotted,
        c.PHUX_UNDERLINE_DASHED => .dashed,
        else => .single,
    };
}

pub fn selection(cells: []const c.PhuxTerminalCell) ?[2]u16 {
    var first: ?u16 = null;
    var last: u16 = 0;
    for (cells, 0..) |raw, col| {
        if (raw.flags & c.PHUX_CLIENT_CELL_SELECTED == 0) continue;
        if (first == null) first = @intCast(col);
        last = @intCast(col);
    }
    return if (first) |start| .{ start, last } else null;
}
