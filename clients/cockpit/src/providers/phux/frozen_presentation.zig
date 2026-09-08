//! An owned display-only projection. It grants no input or replica authority.
const std = @import("std");
const provider = @import("provider_contract");
const CanvasStore = @import("presentation.zig").CanvasStore;

pub const FrozenPresentation = struct {
    gpa: std.mem.Allocator,
    canvas: CanvasStore,
    value: provider.Presentation,

    pub fn create(gpa: std.mem.Allocator, source: *const CanvasStore, value: provider.Presentation) !*FrozenPresentation {
        const self = try gpa.create(FrozenPresentation);
        errdefer gpa.destroy(self);
        var owned = try source.clone(gpa);
        errdefer owned.deinit(gpa);
        const title = try gpa.dupe(u8, value.title);
        self.* = .{ .gpa = gpa, .canvas = owned, .value = value };
        self.value.title = title;
        self.value.phase = .frozen;
        self.value.grid = self.canvas.grid(false);
        return self;
    }

    pub fn destroy(self: *FrozenPresentation) void {
        const gpa = self.gpa;
        self.canvas.deinit(gpa);
        gpa.free(self.value.title);
        gpa.destroy(self);
    }
};
