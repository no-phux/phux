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
    try std.testing.expect(state.acceptsShortcut(.{ .id = "terminal.new", .key = "r", .modifiers = .{ .primary = true, .shift = true } }));
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
