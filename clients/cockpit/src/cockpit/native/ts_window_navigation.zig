//! Open native windows are presentation identities, including empty windows.
//! Their slot is only an address: the lifetime epoch authorizes activation.
const std = @import("std");

pub const request_name = "cockpit.window-command";
pub const page_size = 16;
pub const max_bytes = 8192;
pub const max_target_bytes = 22;
pub const Error = error{ InvalidRequest, StaleRevision, BufferTooSmall, CatalogTooLarge };

pub const Target = struct {
    window: u8,
    epoch: u64,
    tab: ?struct { generation: u64, id: u32 } = null,

    pub fn validWindow(self: Target, model: anytype) bool {
        if (!model.windowOpen(self.window)) return false;
        if (self.epoch == std.math.maxInt(u64)) return false;
        return model.window_epochs[self.window] == self.epoch;
    }

    /// Reordering preserves a tab's identity; removal or replacement refuses.
    pub fn resolve(self: Target, model: anytype) ?Selection {
        if (!self.validWindow(model)) return null;
        const tab = self.tab orelse return .{ .window = self.window };
        const ws = model.wsAtConst(self.window) orelse return null;
        if (tab.generation == std.math.maxInt(u64)) return null;
        if (ws.tab_generation != tab.generation) return null;
        for (ws.tab_ids[0..ws.tab_count], 0..) |id, index| {
            if (id != 0 and id == tab.id) return .{ .window = self.window, .tab = @intCast(index) };
        }
        return null;
    }

    pub fn encode(self: Target, out: *[max_target_bytes]u8) []const u8 {
        out[0] = if (self.tab != null) 5 else 4;
        out[1] = self.window;
        std.mem.writeInt(u64, out[2..10], self.epoch, .little);
        const tab = self.tab orelse return out[0..10];
        std.mem.writeInt(u64, out[10..18], tab.generation, .little);
        std.mem.writeInt(u32, out[18..22], tab.id, .little);
        return out;
    }
};

pub const Selection = struct { window: u8, tab: ?u8 = null };

pub fn decodeTarget(bytes: []const u8) ?Target {
    if (bytes.len != 10 and bytes.len != 22) return null;
    var target: Target = .{ .window = bytes[1], .epoch = std.mem.readInt(u64, bytes[2..10], .little) };
    switch (bytes[0]) {
        4 => if (bytes.len != 10) return null,
        5 => {
            if (bytes.len != 22) return null;
            target.tab = .{ .generation = std.mem.readInt(u64, bytes[10..18], .little), .id = std.mem.readInt(u32, bytes[18..22], .little) };
        },
        else => return null,
    }
    return target;
}

pub const Command = struct { id: u64, target: Target };

pub fn decodeCommand(bytes: []const u8) ?Command {
    if (bytes.len < 20) return null;
    if (bytes[0] != 1 or bytes[1] != 1) return null;
    const id = std.mem.readInt(u64, bytes[2..10], .little);
    if (id == 0) return null;
    return .{ .id = id, .target = decodeTarget(bytes[10..]) orelse return null };
}

/// Called only after decoding. The engine hook cancels older pending focus,
/// ends hidden pointer capture, syncs remote focus, and raises the SDK window.
/// Raising is required even when this is already the model's active window:
/// it may be minimized. The hook must not create a window or terminal.
pub fn activate(engine: anytype, target: Target, fx: anytype) bool {
    const selected = target.resolve(engine.model) orelse return false;
    const ws = engine.model.wsAt(selected.window) orelse return false;
    if (selected.tab) |tab| {
        ws.selected_tab = tab;
        ws.web_selected = false;
    }
    engine.model.active_window = selected.window;
    engine.didSelectNavigationWindow(selected.window, fx);
    return true;
}

const Request = struct { offset: u16, query: []const u8 };

fn parse(revision: u64, bytes: []const u8) Error!Request {
    if (bytes.len < 15) return error.InvalidRequest;
    if (bytes[0] != 1 or bytes[1] != 4) return error.InvalidRequest;
    const end = 13 + @as(usize, bytes[12]);
    if (bytes[12] > 64 or bytes.len != end + 2) return error.InvalidRequest;
    if (bytes[end] != 4 or bytes[end + 1] != 0) return error.InvalidRequest;
    if (!std.unicode.utf8ValidateSlice(bytes[13..end])) return error.InvalidRequest;
    if (std.mem.readInt(u64, bytes[2..10], .little) != revision) return error.StaleRevision;
    return .{ .offset = std.mem.readInt(u16, bytes[10..12], .little), .query = bytes[13..end] };
}

const Row = struct { target: Target, index: u16 };
const Page = struct { rows: [page_size]Row = undefined, count: usize = 0, total: u16 = 0 };

fn append(page: *Page, target: Target, index: usize, request: Request, model: anytype, labels: anytype) Error!void {
    if (!labels.matches(model, target, request.query)) return;
    if (index > std.math.maxInt(u16) or page.total == std.math.maxInt(u16)) return error.CatalogTooLarge;
    if (page.total >= request.offset and page.count < page_size) {
        page.rows[page.count] = .{ .target = target, .index = @intCast(index) };
        page.count += 1;
    }
    page.total += 1;
}

fn collect(model: anytype, request: Request, labels: anytype) Error!Page {
    var page: Page = .{};
    var index: usize = 0;
    for (model.window_epochs, 0..) |epoch, window| {
        if (!model.windowOpen(window)) continue;
        const ws = model.wsAtConst(window) orelse continue;
        const target: Target = .{ .window = @intCast(window), .epoch = epoch };
        try append(&page, target, index, request, model, labels);
        index += 1;
        for (ws.tab_ids[0..ws.tab_count]) |id| {
            var tab = target;
            tab.tab = .{ .generation = ws.tab_generation, .id = id };
            try append(&page, tab, index, request, model, labels);
            index += 1;
        }
    }
    if (request.offset > page.total) return error.InvalidRequest;
    return page;
}

pub fn contains(text: []const u8, query: []const u8) bool {
    if (query.len > text.len) return false;
    for (0..text.len - query.len + 1) |at| {
        if (std.ascii.eqlIgnoreCase(text[at..][0..query.len], query)) return true;
    }
    return false;
}

/// The navigator's existing page framing, with scope 4 and target tags 4/5.
/// Shipping labels are supplied by `window_labels.zig`.
pub fn encodeWithLabels(model: anytype, revision: u64, request: []const u8, out: []u8, labels: anytype) Error![]const u8 {
    const page = try collect(model, try parse(revision, request), labels);
    if (out.len < request.len + 3) return error.BufferTooSmall;
    @memcpy(out[0..request.len], request);
    std.mem.writeInt(u16, out[request.len..][0..2], page.total, .little);
    out[request.len + 2] = @intCast(page.count);
    var at = request.len + 3;
    for (page.rows[0..page.count]) |row| at = try encodeRow(model, row, out, at, labels);
    if (at == out.len) return error.BufferTooSmall;
    out[at] = 0x4e;
    at += 1;
    for (page.rows[0..page.count]) |row| at = try encodeMetadata(model, row.target, out, at, labels);
    return out[0..at];
}

fn encodeRow(model: anytype, row: Row, out: []u8, at: usize, labels: anytype) Error!usize {
    var label_buffer: [240]u8 = undefined;
    var target_buffer: [max_target_bytes]u8 = undefined;
    const label = labels.label(model, row.target, &label_buffer);
    const target = row.target.encode(&target_buffer);
    const end = at + 5 + target.len + label.len;
    if (end > out.len) return error.BufferTooSmall;
    std.mem.writeInt(u16, out[at..][0..2], row.index, .little);
    out[at + 2] = @intCast(label.len);
    std.mem.writeInt(u16, out[at + 3 ..][0..2], @intCast(target.len), .little);
    @memcpy(out[at + 5 ..][0..target.len], target);
    @memcpy(out[at + 5 + target.len ..][0..label.len], label);
    return end;
}

fn encodeMetadata(model: anytype, target: Target, out: []u8, at: usize, labels: anytype) Error!usize {
    var buffer: [160]u8 = undefined;
    const detail = labels.detail(model, target, &buffer);
    const end = at + 3 + detail.len;
    if (end > out.len) return error.BufferTooSmall;
    out[at] = if (target.tab == null) 4 else 5;
    out[at + 1] = @as(u8, @intFromBool(target.resolve(model) != null)) | (@as(u8, @intFromBool(target.window == model.active_window)) << 1);
    out[at + 2] = @intCast(detail.len);
    @memcpy(out[at + 3 ..][0..detail.len], detail);
    return end;
}

pub fn display(text: []const u8, out: []u8) []const u8 {
    if (text.len <= out.len) {
        @memcpy(out[0..text.len], text);
        return out[0..text.len];
    }
    if (out.len < 3) return "";
    var end = out.len - 3;
    while (end > 0 and (text[end] & 0xc0) == 0x80) end -= 1;
    @memcpy(out[0..end], text[0..end]);
    @memcpy(out[end..][0..3], "…");
    return out[0 .. end + 3];
}

const Fixture = struct {
    const Workspace = struct { tab_ids: [2]u32 = .{ 11, 12 }, tab_count: usize = 2, tab_generation: u64 = 7, selected_tab: u8 = 0, web_selected: bool = false };
    window_epochs: [3]u64 = .{ 1, 2, 3 },
    open: [3]bool = @splat(true),
    workspaces: [3]Workspace = .{ .{}, .{ .tab_count = 0 }, .{} },
    active_window: usize = 0,
    pub fn windowOpen(self: *const Fixture, index: usize) bool {
        return index < self.open.len and self.open[index];
    }
    pub fn wsAtConst(self: *const Fixture, index: usize) ?*const Workspace {
        return if (self.windowOpen(index)) &self.workspaces[index] else null;
    }
    pub fn wsAt(self: *Fixture, index: usize) ?*Workspace {
        return if (self.windowOpen(index)) &self.workspaces[index] else null;
    }
};

const TestLabels = struct {
    fn matches(self: TestLabels, model: anytype, target: Target, query: []const u8) bool {
        var out: [160]u8 = undefined;
        return contains(self.label(model, target, &out), query) or contains(self.detail(model, target, &out), query);
    }
    fn label(_: TestLabels, _: anytype, _: Target, _: []u8) []const u8 {
        return "Same title";
    }
    fn detail(_: TestLabels, _: anytype, target: Target, out: []u8) []const u8 {
        return std.fmt.bufPrint(out, "Window {d}", .{target.window + 1}) catch unreachable;
    }
};

test "empty minimized window activation raises exact identity even when already active" {
    const Engine = struct {
        model: *Fixture,
        raised: ?usize = null,
        calls: usize = 0,
        pub fn didSelectNavigationWindow(self: *@This(), window: usize, _: anytype) void {
            self.raised = window;
            self.calls += 1;
        }
    };
    var model: Fixture = .{};
    var engine: Engine = .{ .model = &model };
    const target: Target = .{ .window = 1, .epoch = 2 };
    try std.testing.expect(activate(&engine, target, .{}));
    try std.testing.expectEqual(@as(?usize, 1), engine.raised);
    try std.testing.expect(activate(&engine, target, .{}));
    try std.testing.expectEqual(@as(usize, 2), engine.calls);
    try std.testing.expectEqual(@as(usize, 0), model.workspaces[1].tab_count);
    model.window_epochs[1] += 1; // close and reopen the same SDK slot
    try std.testing.expect(!activate(&engine, target, .{}));
    try std.testing.expectEqual(@as(usize, 2), engine.calls);
}

test "tab activation follows identity through reorder but refuses removed and replaced tabs" {
    var model: Fixture = .{};
    const target: Target = .{ .window = 2, .epoch = 3, .tab = .{ .generation = 7, .id = 11 } };
    model.workspaces[2].tab_ids = .{ 12, 11 };
    try std.testing.expectEqual(@as(?u8, 1), target.resolve(&model).?.tab);
    model.workspaces[2].tab_ids[1] = 13;
    try std.testing.expect(target.resolve(&model) == null);
    model.workspaces[2].tab_ids[1] = 11;
    model.workspaces[2].tab_generation += 1;
    try std.testing.expect(target.resolve(&model) == null);
    try std.testing.expect((Target{ .window = 255, .epoch = 0 }).resolve(&model) == null);
}

test "window catalog includes duplicate titles and empty windows with stable target and current marker" {
    var model: Fixture = .{};
    var request = [_]u8{0} ** 15;
    request[0] = 1;
    request[1] = 4;
    request[2] = 9;
    request[13] = 4;
    var buffer: [max_bytes]u8 = undefined;
    const result = try encodeWithLabels(&model, 9, &request, &buffer, TestLabels{});
    try std.testing.expectEqual(@as(u16, 7), std.mem.readInt(u16, result[15..17], .little));
    try std.testing.expectEqual(@as(u8, 7), result[17]);
    var at: usize = 18;
    for (0..7) |index| {
        const len = std.mem.readInt(u16, result[at + 3 ..][0..2], .little);
        const target = decodeTarget(result[at + 5 ..][0..len]).?;
        if (index == 3) {
            try std.testing.expectEqual(@as(u8, 1), target.window);
            try std.testing.expect(target.tab == null);
        }
        at += 5 + len + result[at + 2];
    }
    try std.testing.expectEqual(@as(u8, 0x4e), result[at]);
    try std.testing.expectEqual(@as(u8, 3), result[at + 2]);
    try std.testing.expectError(error.StaleRevision, encodeWithLabels(&model, 10, &request, &buffer, TestLabels{}));
    try std.testing.expectError(error.BufferTooSmall, encodeWithLabels(&model, 9, &request, buffer[0..18], TestLabels{}));
}
