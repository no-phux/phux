//! Tabs own trees; trees own panes. These pin the behavior the two-pane
//! attachment model could not express, and the ONE geometry derivation that
//! the painter, the hit targets, and the PTY pump all share.

const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const support = @import("support.zig");

const geometry = native_sdk.geometry;
const testing = std.testing;

const createSession = support.createSession;

const surface = geometry.SizeF.init(980, 640);

test "a fresh window is one tab of one pane filling the content area" {
    const session = try createSession(80, 24);
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
    try testing.expect(model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));

    var panes: [app.max_panes_per_tab]app.LayoutPane = undefined;
    const count = app.resolvePanes(&model, surface, &panes);
    try testing.expectEqual(@as(usize, 1), count);
    const content = app.workspaceChrome(&model, surface).content;
    try testing.expectEqualDeep(content, panes[0].rect);

    // The web surface takes the content area away from every pane.
    model.selectWeb();
    try testing.expectEqual(@as(usize, 0), app.resolvePanes(&model, surface, &panes));
}

test "the tab band is at least as tall as the triggers it hosts" {
    // At 40pt the strip overflowed the band and painted its hairline and
    // underline indicator into the terminal's first row.
    const session = try createSession(80, 24);
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    try testing.expect(app.header_height >= app.tabTriggerHeight(&model));
}
