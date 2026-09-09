//! Pointer-free source colors retained for allocation-free theme repaint,
//! including frozen presentations whose originating C client is gone.
const std = @import("std");
const canvas = @import("native_sdk").canvas;
const c = @import("abi.zig").c;
const projection = @import("cell_projection.zig");
const metadata = @import("grid_metadata.zig");

const Cell = struct {
    raw: c.PhuxTerminalCell,
    provenance: c.PhuxGridCellMetadata,
};

pub const OwnedColors = struct {
    cells: std.ArrayListUnmanaged(Cell) = .empty,
    globals: ?c.PhuxTerminalGridMetadata = null,

    pub fn deinit(self: *OwnedColors, gpa: std.mem.Allocator) void {
        self.cells.deinit(gpa);
    }

    pub fn reserve(self: *OwnedColors, gpa: std.mem.Allocator, count: usize) !void {
        try self.cells.ensureTotalCapacity(gpa, count);
    }

    /// Called only after admission and ALL allocations succeed.
    pub fn capture(self: *OwnedColors, raw: []const c.PhuxTerminalCell, meta: ?*const c.PhuxTerminalGridMetadata) void {
        self.cells.clearRetainingCapacity();
        self.globals = null;
        const source = meta orelse return;
        self.globals = source.*;
        self.globals.?.cells = null;
        for (raw, 0..) |cell, index| self.cells.appendAssumeCapacity(.{
            .raw = cell,
            .provenance = source.cells[index],
        });
    }

    pub fn recolor(self: *const OwnedColors, cells: []canvas.TerminalCell, policy: metadata.Policy) void {
        const globals = &(self.globals orelse return);
        // CanvasStore commits both lengths from the admitted view count, with
        // no fallible work between capture and publication. Callers must keep
        // this pairing intact; neither slice may be resized independently.
        std.debug.assert(self.cells.items.len == cells.len);
        for (self.cells.items, cells) |source, *target| {
            // Colors are recomputed from ORIGINAL RGB/flags, never from an
            // already inversed/faintened cell. Glyphs and decorations stay owned.
            const resolved = projection.project(source.raw, "", .{
                .grid = globals,
                .cell = source.provenance,
                .policy = policy,
            });
            target.fg = resolved.fg;
            target.bg = resolved.bg;
            target.underline_color = resolved.underline_color;
        }
    }
};
