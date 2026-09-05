const std = @import("std");
const app = @import("../native_test_root.zig");

const testing = std.testing;

test "tab projection ids survive moves and follow their tab through removal" {
    var workspace: app.Workspace = .{};
    try testing.expect(workspace.admitTab(app.initialTerminalRef(0)));
    try testing.expect(workspace.admitTab(app.initialTerminalRef(1)));
    try testing.expect(workspace.admitTab(app.initialTerminalRef(2)));

    const first = workspace.tabId(0).?;
    const second = workspace.tabId(1).?;
    const third = workspace.tabId(2).?;
    try testing.expect(first != second and second != third and first != third);

    try testing.expect(workspace.moveTerminal(app.initialTerminalRef(1), -1));
    try testing.expectEqual(second, workspace.tabId(0).?);
    try testing.expectEqual(first, workspace.tabId(1).?);

    workspace.dropTab(1);
    try testing.expectEqual(second, workspace.tabId(0).?);
    try testing.expectEqual(third, workspace.tabId(1).?);
    try testing.expect(workspace.tabId(2) == null);
}

test "restored tabs receive nonzero process-local projection ids" {
    var workspace: app.Workspace = .{};
    workspace.tab_count = 2;
    workspace.tabs[0] = app.Tree.initLeaf(app.initialTerminalRef(0));
    workspace.tabs[1] = app.Tree.initLeaf(app.initialTerminalRef(1));

    workspace.assignRestoredTabId(0);
    workspace.assignRestoredTabId(1);
    try testing.expect(workspace.tabId(0).? != 0);
    try testing.expect(workspace.tabId(1).? != 0);
    try testing.expect(workspace.tabId(0).? != workspace.tabId(1).?);
}
