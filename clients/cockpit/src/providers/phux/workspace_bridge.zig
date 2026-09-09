//! Transactional owning copy of the C workspace publication. No emulator slots.
const std = @import("std");
const contract = @import("provider_contract");
const ws = contract.workspace;
const c = @import("abi.zig").c;

pub fn record(comptime T: type) T {
    var raw = std.mem.zeroes(T);
    raw.size = @sizeOf(T);
    raw.version = c.PHUX_CLIENT_ABI_VERSION;
    return raw;
}

fn check(result: c.PhuxClientResult) !void {
    if (result != c.PHUX_CLIENT_OK) return error.WorkspaceRead;
}

pub fn text(raw: c.PhuxBytes) !ws.Text {
    if (raw.len > ws.max_text_bytes) return error.TextTooLong;
    if (raw.len == 0) return .{};
    if (raw.data == null) return error.InvalidIdentity;
    const value = raw.data[0..raw.len];
    if (!std.unicode.utf8ValidateSlice(value)) return error.InvalidIdentity;
    if (std.mem.indexOfScalar(u8, value, 0) != null) return error.InvalidIdentity;
    return ws.Text.init(value);
}

pub fn terminalRef(raw: c.PhuxTerminalId) !contract.TerminalRef {
    const host_name = try text(raw.host);
    switch (raw.kind) {
        c.PHUX_TERMINAL_LOCAL => if (host_name.len != 0) return error.InvalidIdentity,
        c.PHUX_TERMINAL_SATELLITE => if (host_name.len == 0) return error.InvalidIdentity,
        else => return error.InvalidIdentity,
    }
    if (raw.id == 0) return error.InvalidIdentity;
    return .{ .provider_id = .phux, .terminal_id = .{
        .phux = try contract.RemoteTerminalId.fromPhux(raw.kind, raw.id, host_name.slice()),
    } };
}

pub fn rawTerminal(ref: *const ?contract.TerminalRef) !c.PhuxTerminalId {
    const value = if (ref.*) |*value| value else return std.mem.zeroes(c.PhuxTerminalId);
    if (value.provider_id != .phux) return error.InvalidIdentity;
    const remote = switch (value.terminal_id) {
        .phux => |*id| id,
        else => return error.InvalidIdentity,
    };
    const raw: c.PhuxTerminalId = .{ .kind = remote.kind, .id = remote.id, .host = bytes(remote.host()) };
    _ = try terminalRef(raw);
    return raw;
}

fn bytes(value: []const u8) c.PhuxBytes {
    return .{ .data = if (value.len == 0) null else value.ptr, .len = value.len };
}

/// Returned pointers borrow the caller-owned mutation, never a local ID copy.
pub fn mutation(value: *const ws.Mutation, request_id: u32) !c.PhuxWorkspaceMutation {
    _ = try text(bytes(value.name));
    var raw = record(c.PhuxWorkspaceMutation);
    raw.request_id = request_id;
    raw.expected_revision = value.expected_revision;
    raw.session_id = value.session_id;
    raw.kind = @intFromEnum(value.kind);
    raw.window_id = value.window_id;
    raw.terminal_id = try rawTerminal(&value.terminal_ref);
    raw.new_terminal_id = try rawTerminal(&value.new_terminal_ref);
    raw.name = bytes(value.name);
    raw.direction = @intFromEnum(value.direction);
    raw.index = value.index;
    raw.ratio = value.ratio;
    raw.path_len = value.path_len;
    raw.path_bits = value.path_bits;
    return raw;
}

pub const Store = struct {
    info: ws.Snapshot = .{},
    message: ws.Text = .{},
    windows: []ws.Window = &.{},
    nodes: []ws.Node = &.{},
    catalog: []ws.CatalogTerminal = &.{},

    pub fn deinit(self: *Store, gpa: std.mem.Allocator) void {
        gpa.free(self.windows);
        gpa.free(self.nodes);
        gpa.free(self.catalog);
        self.* = .{};
    }

    pub fn snapshot(self: *const Store) ws.Snapshot {
        var result = self.info;
        result.message = self.message.slice();
        result.windows = self.windows;
        result.nodes = self.nodes;
        return result;
    }

    pub fn refuse(self: *Store, err: anyerror) void {
        self.info.state = .last_good_error;
        self.info.status = .refused;
        self.message = ws.Text.init(@errorName(err)) catch unreachable;
    }

    pub fn contains(self: *const Store, ref: contract.TerminalRef) bool {
        for (self.catalog) |*entry| if (entry.terminal_ref.eql(ref)) return true;
        return false;
    }

    pub fn terminalSession(self: *const Store, ref: contract.TerminalRef) ?u32 {
        for (self.catalog) |*entry| {
            if (entry.terminal_ref.eql(ref)) return if (entry.session_id == 0) null else entry.session_id;
        }
        return null;
    }

    pub fn capture(self: *Store, gpa: std.mem.Allocator, client: *const c.PhuxClient) !bool {
        var raw = record(c.PhuxWorkspaceInfo);
        try check(c.phux_client_workspace_info(client, &raw));
        return self.captureInfo(gpa, client, raw);
    }

    /// Public for adversarial ABI capacity tests. Adoption is all-or-nothing.
    pub fn captureInfo(self: *Store, gpa: std.mem.Allocator, client: *const c.PhuxClient, raw: c.PhuxWorkspaceInfo) !bool {
        try validateCounts(raw);
        const message = try text(raw.message);
        const info = try infoFromC(raw);
        if (std.meta.eql(self.info, info) and std.mem.eql(u8, self.message.slice(), message.slice())) return false;
        if (self.samePublication(info)) {
            // Pending/refused/disconnected status must publish even under
            // allocation pressure. The revision fences these existing arrays.
            self.info = info;
            self.message = message;
            return true;
        }
        var next: Store = .{ .info = info, .message = message };
        errdefer next.deinit(gpa);
        try next.copyRecords(gpa, client, raw);
        self.deinit(gpa);
        self.* = next;
        return true;
    }

    fn samePublication(self: *const Store, info: ws.Snapshot) bool {
        return self.info.revision != 0 and self.info.revision == info.revision and self.info.session_id == info.session_id;
    }

    fn copyRecords(self: *Store, gpa: std.mem.Allocator, client: *const c.PhuxClient, info: c.PhuxWorkspaceInfo) !void {
        self.windows = try gpa.alloc(ws.Window, info.window_count);
        self.nodes = try gpa.alloc(ws.Node, info.node_count);
        self.catalog = try gpa.alloc(ws.CatalogTerminal, info.terminal_count);
        for (self.windows, 0..) |*window, index| {
            var raw = record(c.PhuxWorkspaceWindow);
            try check(c.phux_client_workspace_window_get(client, index, &raw));
            if (raw.root_node >= info.node_count) return error.InvalidWorkspace;
            window.* = .{ .id = raw.window_id, .name = try text(raw.name), .root = raw.root_node };
        }
        for (self.nodes, 0..) |*node, index| {
            var raw = record(c.PhuxWorkspaceNode);
            try check(c.phux_client_workspace_node_get(client, index, &raw));
            node.* = try nodeFromC(raw, info.node_count);
        }
        for (self.catalog, 0..) |*entry, index| {
            var raw = record(c.PhuxCatalogTerminal);
            try check(c.phux_client_catalog_terminal_get(client, index, &raw));
            entry.* = .{
                .terminal_ref = try terminalRef(raw.terminal_id),
                .session_id = raw.session_id,
                .title = try text(raw.title),
                .cwd = try text(raw.cwd),
            };
        }
    }
};

fn validateCounts(raw: c.PhuxWorkspaceInfo) !void {
    if (raw.window_count > ws.max_windows) return error.WorkspaceCapacity;
    if (raw.node_count > ws.max_nodes) return error.WorkspaceCapacity;
    if (raw.terminal_count > ws.max_terminals) return error.WorkspaceCapacity;
}

fn infoFromC(raw: c.PhuxWorkspaceInfo) !ws.Snapshot {
    return .{
        .revision = raw.revision,
        .session_id = raw.session_id,
        .state = std.enums.fromInt(ws.State, raw.state) orelse return error.InvalidWorkspace,
        .status = std.enums.fromInt(ws.Status, raw.status) orelse return error.InvalidWorkspace,
        .request_id = raw.request_id,
    };
}

fn nodeFromC(raw: c.PhuxWorkspaceNode, count: u32) !ws.Node {
    if (raw.kind == 1) return .{ .kind = .leaf, .terminal_ref = try terminalRef(raw.terminal_id) };
    try validateSplit(raw, count);
    return .{ .kind = switch (raw.kind) {
        2 => .horizontal,
        3 => .vertical,
        else => return error.InvalidWorkspace,
    }, .first = raw.first, .second = raw.second, .ratio = raw.ratio };
}

fn validateSplit(raw: c.PhuxWorkspaceNode, count: u32) !void {
    if (raw.first >= count or raw.second >= count) return error.InvalidWorkspace;
    if (!std.math.isFinite(raw.ratio) or raw.ratio <= 0 or raw.ratio >= 1) return error.InvalidWorkspace;
}

test "workspace mutation maps every field and borrows the owning satellite IDs" {
    const ref: contract.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{
        .phux = try contract.RemoteTerminalId.fromPhux(c.PHUX_TERMINAL_SATELLITE, 91, "remote-builder"),
    } };
    const value: ws.Mutation = .{
        .expected_revision = 44,
        .session_id = 12,
        .kind = .split,
        .window_id = @splat(8),
        .terminal_ref = ref,
        .new_terminal_ref = ref,
        .name = "build",
        .index = 3,
        .direction = .vertical,
        .ratio = 0.25,
        .path_bits = 5,
        .path_len = 3,
    };
    const raw = try mutation(&value, 71);
    try std.testing.expectEqual(@sizeOf(c.PhuxWorkspaceMutation), raw.size);
    try std.testing.expectEqual(c.PHUX_CLIENT_ABI_VERSION, raw.version);
    try std.testing.expectEqual(@as(u32, 71), raw.request_id);
    try std.testing.expectEqual(@as(u64, 44), raw.expected_revision);
    try std.testing.expectEqual(@as(u32, 12), raw.session_id);
    try std.testing.expectEqual(@as(u32, 2), raw.kind);
    try std.testing.expectEqualSlices(u8, &value.window_id, &raw.window_id);
    try std.testing.expect((try terminalRef(raw.terminal_id)).eql(ref));
    try std.testing.expect((try terminalRef(raw.new_terminal_id)).eql(ref));
    try std.testing.expectEqual(@intFromPtr(value.terminal_ref.?.terminal_id.phux.host().ptr), @intFromPtr(raw.terminal_id.host.data));
    try std.testing.expectEqual(@intFromPtr(value.new_terminal_ref.?.terminal_id.phux.host().ptr), @intFromPtr(raw.new_terminal_id.host.data));
    try std.testing.expectEqualStrings("build", raw.name.data[0..raw.name.len]);
    try std.testing.expectEqual(@as(u32, 3), raw.index);
    try std.testing.expectEqual(@as(u32, 3), raw.direction);
    try std.testing.expectEqual(@as(f32, 0.25), raw.ratio);
    try std.testing.expectEqual(@as(u32, 3), raw.path_len);
    try std.testing.expectEqual(@as(u64, 5), raw.path_bits);
}

test "workspace text and identity copies reject overflow without truncation" {
    var source = [_]u8{'x'} ** (ws.max_text_bytes + 1);
    const good = try text(bytes(source[0..ws.max_text_bytes]));
    source[0] = 'y';
    try std.testing.expectEqual(@as(u8, 'x'), good.slice()[0]);
    try std.testing.expectError(error.TextTooLong, text(bytes(&source)));
    try std.testing.expectError(error.HostTooLong, terminalRef(.{ .kind = c.PHUX_TERMINAL_SATELLITE, .id = 1, .host = bytes(source[0..256]) }));
    try std.testing.expectError(error.InvalidIdentity, text(.{ .data = null, .len = 1 }));
    var host_name = "build".*;
    const ref = try terminalRef(.{ .kind = c.PHUX_TERMINAL_SATELLITE, .id = 1, .host = bytes(&host_name) });
    host_name[0] = 'x';
    try std.testing.expectEqualStrings("build", ref.terminal_id.phux.host());
}

test "workspace split ratios require two nonempty panes" {
    var raw = record(c.PhuxWorkspaceNode);
    raw.kind = 2;
    raw.first = 0;
    raw.second = 1;
    raw.ratio = 0;
    try std.testing.expectError(error.InvalidWorkspace, nodeFromC(raw, 2));
    raw.ratio = 1;
    try std.testing.expectError(error.InvalidWorkspace, nodeFromC(raw, 2));
    raw.ratio = 0.5;
    try std.testing.expectEqual(@as(f32, 0.5), (try nodeFromC(raw, 2)).ratio);
}
