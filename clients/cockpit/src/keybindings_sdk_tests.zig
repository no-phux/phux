const std = @import("std");
/// The shipping extension calls this with the engine-exported runtime module.
/// Importing that file again from the extension would give Config.keybindings
/// and the registry different module identities.
pub fn check(comptime sdk: type, comptime runtime: type) !void {
    const menus = [_]sdk.platform.Menu{.{ .title = "Shell", .items = &.{
        .{ .command = "terminal.new", .label = "New Tab", .key = "t", .modifiers = .{ .primary = true } },
    } }};
    var state = try runtime.State(sdk.platform).init(&.{}, &menus);
    var platform = sdk.platform.NullPlatform.init(.{});
    var overrides: runtime.bindings.Overrides = .{};
    try state.sync(platform.platform().services, &overrides, true);
    try std.testing.expectEqualStrings("t", platform.configuredShortcuts()[0].key);
    try std.testing.expectEqualStrings("terminal.new", state.commandForEvent(sdk.canvas.WidgetKeyboardEvent{
        .phase = .key_down,
        .key = "t",
        .modifiers = .{ .super = true },
    }).?);
    try overrides.set("terminal.new", "cmd+shift+r");
    try state.sync(platform.platform().services, &overrides, true);
    try std.testing.expectEqualStrings("r", platform.configuredShortcuts()[0].key);
    try std.testing.expect(platform.configuredShortcuts()[0].modifiers.shift);
    try std.testing.expectEqualStrings("r", platform.configuredMenus()[0].items[0].key);
    try std.testing.expect(!state.acceptsShortcut(.{ .id = "terminal.new", .key = "t", .modifiers = .{ .primary = true } }));
    const registered_id = platform.configuredShortcuts()[0].id;
    try std.testing.expectEqualStrings("terminal.new", state.commandForShortcut(.{ .id = registered_id, .key = "r", .modifiers = .{ .primary = true, .shift = true } }).?);
    try std.testing.expect(!state.acceptsShortcut(.{ .id = "other.command", .key = "r", .modifiers = .{ .primary = true, .shift = true } }));
    try std.testing.expect(state.commandForEvent(sdk.canvas.WidgetKeyboardEvent{
        .phase = .key_down,
        .key = "t",
        .modifiers = .{ .super = true },
    }) == null);
    try std.testing.expectEqualStrings("terminal.new", state.commandForEvent(sdk.canvas.WidgetKeyboardEvent{
        .phase = .key_down,
        .key = "R",
        .modifiers = .{ .super = true, .shift = true },
    }).?);
    try std.testing.expect(state.commandForEvent(sdk.canvas.WidgetKeyboardEvent{
        .phase = .key_up,
        .key = "r",
        .modifiers = .{ .super = true, .shift = true },
    }) == null);
    try std.testing.expect(state.commandForEvent(sdk.canvas.WidgetKeyboardEvent{
        .phase = .key_down,
        .key = "r",
        .modifiers = .{ .control = true, .shift = true },
    }) == null);
    try state.sync(platform.platform().services, &overrides, false);
    try std.testing.expectEqual(@as(usize, 0), platform.configuredShortcuts().len);
    try std.testing.expectEqualStrings("", platform.configuredMenus()[0].items[0].key);
    overrides.resetAll();
    try state.sync(platform.platform().services, &overrides, true);
    try std.testing.expectEqualStrings("t", platform.configuredShortcuts()[0].key);
}

pub fn checkStorage(comptime sdk: type, comptime runtime: type) !void {
    const menus = [_]sdk.platform.Menu{.{ .title = "Shell", .items = &.{
        .{ .command = "terminal.new", .label = "New Tab", .key = "t", .modifiers = .{ .primary = true } },
    } }};
    var state = try runtime.State(sdk.platform).init(&.{}, &menus);
    var platform = sdk.platform.NullPlatform.init(.{});
    var overrides: runtime.bindings.Overrides = .{};
    try overrides.set("terminal.new", "cmd+r");
    try state.sync(platform.platform().services, &overrides, true);
    try expectOwned(&state, platform.configuredShortcuts()[0].key);
    try expectOwned(&state, platform.configuredShortcuts()[0].id);
    try expectOwned(&state, platform.configuredMenus()[0].items[0].key);
    var failing: FailMenus(sdk) = .{ .services = platform.platform().services };
    try overrides.set("terminal.new", "cmd+y");
    try std.testing.expectError(error.TestMenuFailed, state.sync(&failing, &overrides, true));
    try expectOwned(&state, platform.configuredShortcuts()[0].key);
    try expectOwned(&state, platform.configuredShortcuts()[0].id);
    try expectOwned(&state, platform.configuredMenus()[0].items[0].key);
    churnStack();
    try std.testing.expectEqualStrings("r", platform.configuredShortcuts()[0].key);
}

noinline fn churnStack() void {
    var bytes: [65536]u8 = undefined;
    @memset(&bytes, 0xaa);
    std.mem.doNotOptimizeAway(&bytes);
}

fn FailMenus(comptime sdk: type) type {
    return struct {
        services: sdk.platform.PlatformServices,
        fail_once: bool = true,
        pub fn configureShortcuts(self: *@This(), shortcuts: []const sdk.platform.Shortcut) !void {
            try self.services.configureShortcuts(shortcuts);
        }
        pub fn configureMenus(self: *@This(), menus: []const sdk.platform.Menu) !void {
            if (self.fail_once) {
                self.fail_once = false;
                return error.TestMenuFailed;
            }
            try self.services.configureMenus(menus);
        }
    };
}

fn expectOwned(owner: anytype, bytes: []const u8) !void {
    const start = @intFromPtr(owner);
    const end = start + @sizeOf(@TypeOf(owner.*));
    try std.testing.expect(@intFromPtr(bytes.ptr) >= start);
    try std.testing.expect(@intFromPtr(bytes.ptr) + bytes.len <= end);
}

pub fn checkStale(comptime sdk: type, comptime runtime: type) !void {
    const shortcuts = [_]sdk.platform.Shortcut{
        .{ .id = "pane.previous", .key = "[", .modifiers = .{ .primary = true } },
        .{ .id = "tab.previous", .key = "[", .modifiers = .{ .primary = true, .shift = true } },
    };
    var state = try runtime.State(sdk.platform).init(&shortcuts, &.{});
    var platform = sdk.platform.NullPlatform.init(.{});
    var overrides: runtime.bindings.Overrides = .{};
    try overrides.set("tab.previous", "none");
    try state.sync(platform.platform().services, &overrides, true);
    const old = platform.configuredShortcuts()[0];
    const old_id = try runtime.bindings.Text(64).init(old.id);
    // AppKit emits the matched registration's modifiers, not the physically
    // pressed Shift. Revalidating only chord/id cannot recover that fact.
    const queued: sdk.platform.ShortcutEvent = .{ .id = old_id.slice(), .key = "[", .modifiers = old.modifiers };
    overrides.resetAll();
    try state.sync(platform.platform().services, &overrides, true);
    try std.testing.expect(!state.acceptsShortcut(queued));

    const current_id = try runtime.bindings.Text(64).init(platform.configuredShortcuts()[0].id);
    const before_overlay: sdk.platform.ShortcutEvent = .{ .id = current_id.slice(), .key = "[", .modifiers = old.modifiers };
    try state.sync(platform.platform().services, &overrides, false);
    try state.sync(platform.platform().services, &overrides, true);
    try std.testing.expect(!state.acceptsShortcut(before_overlay));
}

pub fn checkTextPhase(comptime sdk: type, comptime runtime: type) !void {
    const shortcuts = [_]sdk.platform.Shortcut{.{ .id = "terminal.new", .key = "t", .modifiers = .{ .primary = true } }};
    var state = try runtime.State(sdk.platform).init(&shortcuts, &.{});
    var platform = sdk.platform.NullPlatform.init(.{});
    try state.sync(platform.platform().services, &.{}, true);
    try std.testing.expect(state.commandForEvent(sdk.canvas.WidgetKeyboardEvent{
        .phase = .text_input,
        .key = "t",
        .text = "t",
        .modifiers = .{ .super = true },
    }) == null);
}
