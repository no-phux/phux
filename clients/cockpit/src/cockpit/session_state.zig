//! Bounded, nonrecursive, line-oriented workspace state. A required terminator
//! rejects truncated writes; topology validation owns the structural claims.
const std = @import("std");
const local = @import("../providers/local/provider.zig");
const contract = @import("provider_contract");
const layout = @import("layout.zig");
const topology = @import("topology.zig");
const attachments = topology.attachments;

pub const TopologySnapshot = topology.TopologySnapshot;
pub const PersistedTopologySnapshot = topology.PersistedTopologySnapshot;
pub const SnapshotTab = topology.SnapshotTab;
pub const SnapshotCwd = topology.SnapshotCwd;
pub const SnapshotSelection = topology.SnapshotSelection;
pub const TabPlacement = topology.TabPlacement;
pub const magic = "phux-cockpit-state";
pub const terminator = "end";
pub const file_name = if (@import("builtin").mode == .Debug) "workspace-dev.state" else "workspace.state";
pub const release_file_name = "workspace.state";
// 32 refs * (2 * (255 host + 256 endpoint + 255 incarnation) + 128
// scalar/keyword bytes), plus 32 cwd lines and at most 63 live nodes, fits
// below 72 KiB. The fixed 96 KiB ceiling also bounds hostile input parsing.
pub const max_state_bytes: usize = 96 * 1024;
pub const min_readable_version: u16 = 2;

pub fn joinPath(state_dir: []const u8, out: []u8) error{NoSpaceLeft}![]const u8 {
    const separator: []const u8 = if (state_dir.len > 0 and state_dir[state_dir.len - 1] == '/') "" else "/";
    return std.fmt.bufPrint(out, "{s}{s}{s}", .{ state_dir, separator, file_name });
}

const Emitter = struct {
    out: []u8,
    len: usize = 0,

    fn print(self: *Emitter, comptime fmt: []const u8, args: anytype) error{NoSpaceLeft}!void {
        self.len += (try std.fmt.bufPrint(self.out[self.len..], fmt, args)).len;
    }

    fn raw(self: *Emitter, bytes: []const u8) error{NoSpaceLeft}!void {
        if (self.len + bytes.len > self.out.len) return error.NoSpaceLeft;
        @memcpy(self.out[self.len..][0..bytes.len], bytes);
        self.len += bytes.len;
    }

    fn nodeId(self: *Emitter, id: layout.NodeId) error{NoSpaceLeft}!void {
        if (id == layout.none) return self.raw("-");
        return self.print("{d}", .{id});
    }

    fn hex(self: *Emitter, bytes: []const u8) error{NoSpaceLeft}!void {
        if (bytes.len == 0) return self.raw("-");
        for (bytes) |byte| try self.print("{x:0>2}", .{byte});
    }
};

pub const SerializeError = error{ NoSpaceLeft, UnpersistableTerminal };

pub fn serialize(snapshot: *const TopologySnapshot, out: []u8) SerializeError![]const u8 {
    snapshot.validate() catch return error.UnpersistableTerminal;
    var emitter: Emitter = .{ .out = out[0..@min(out.len, max_state_bytes)] };
    try emitter.print("{s} {d}\nplacement {s}\n", .{ magic, snapshot.version, @tagName(snapshot.tab_placement) });
    for (snapshot.references.entries[0..snapshot.references.count], 0..) |entry, index| {
        try emitReference(&emitter, @intCast(index), entry.?);
    }
    for (snapshot.windows[0..snapshot.window_count], 0..) |window, index| {
        switch (window.selection) {
            .web => try emitter.raw("window web\n"),
            .tab => |selected| try emitter.print("window tab {d}\n", .{selected}),
        }
        for (snapshot.windowTabs(index)) |tab| try emitTab(&emitter, tab);
    }
    for (snapshot.cwds, 0..) |cwd, offset| {
        if (cwd.len != 0) try emitter.print("cwd {d} {s}\n", .{ offset, cwd.slice() });
    }
    try emitter.print("{s}\n", .{terminator});
    return emitter.out[0..emitter.len];
}

fn emitReference(emitter: *Emitter, index: u8, entry: attachments.Reference) !void {
    const remote = entry.terminal_ref.terminal_id.phux;
    try emitter.print("ref {d} {d} {d} {d} ", .{ index, @intFromEnum(entry.terminal_ref.provider_id), remote.kind, remote.id });
    try emitter.hex(remote.host());
    try emitter.raw(" ");
    try emitter.hex(entry.context.endpoint.slice());
    try emitter.raw(" ");
    try emitter.hex(entry.context.server_id.slice());
    try emitter.print(" {d}\n", .{entry.context.session_id});
}

fn emitTab(emitter: *Emitter, tab: SnapshotTab) SerializeError!void {
    try emitter.raw("tab ");
    try emitter.nodeId(tab.root);
    try emitter.raw(" ");
    try emitter.nodeId(tab.focus);
    try emitter.raw("\n");
    for (tab.nodes, 0..) |node, index| {
        if (node.kind != .free) try emitNode(emitter, node, index);
    }
}

fn emitNode(emitter: *Emitter, node: topology.SnapshotNode, index: usize) SerializeError!void {
    const kind = if (node.remote_ref != null) "remote" else @tagName(node.kind);
    try emitter.print("node {d} {s} ", .{ index, kind });
    try emitter.nodeId(node.parent);
    if (node.kind == .leaf) {
        const offset = node.remote_ref orelse topology.terminalOffset(node.terminal) orelse return error.UnpersistableTerminal;
        return emitter.print(" {d}\n", .{offset});
    }
    try emitter.print(" {s} {d} ", .{ @tagName(node.orientation), node.fraction });
    try emitter.nodeId(node.first);
    try emitter.raw(" ");
    try emitter.nodeId(node.second);
    try emitter.raw("\n");
}

const Fields = std.mem.TokenIterator(u8, .scalar);
const ParseError = error{InvalidState};

fn token(fields: *Fields) ParseError![]const u8 {
    return fields.next() orelse error.InvalidState;
}

fn number(comptime T: type, fields: *Fields) ParseError!T {
    return std.fmt.parseInt(T, try token(fields), 10) catch error.InvalidState;
}

fn finish(fields: *Fields) ParseError!void {
    if (fields.next() != null) return error.InvalidState;
}

fn nodeId(fields: *Fields) ParseError!layout.NodeId {
    const text = try token(fields);
    if (std.mem.eql(u8, text, "-")) return layout.none;
    const value = std.fmt.parseInt(layout.NodeId, text, 10) catch return error.InvalidState;
    if (value >= layout.max_nodes) return error.InvalidState;
    return value;
}

fn localOffset(fields: *Fields) ParseError!usize {
    const value = try number(usize, fields);
    if (value >= local.max_terminals) return error.InvalidState;
    return value;
}

fn selection(fields: *Fields) ParseError!SnapshotSelection {
    const kind = try token(fields);
    const result: SnapshotSelection = if (std.mem.eql(u8, kind, "web")) .web else if (std.mem.eql(u8, kind, "tab"))
        .{ .tab = try number(u8, fields) }
    else
        return error.InvalidState;
    try finish(fields);
    return result;
}

fn hex(fields: *Fields, out: []u8) ParseError![]const u8 {
    const text = try token(fields);
    if (std.mem.eql(u8, text, "-")) return out[0..0];
    if (text.len / 2 > out.len or text.len % 2 != 0) return error.InvalidState;
    return std.fmt.hexToBytes(out, text) catch error.InvalidState;
}

const Sink = struct {
    tabs: []SnapshotTab,
    tab_count: *u8,
    selection: ?*SnapshotSelection = null,
    windows: ?*[topology.max_snapshot_windows]topology.SnapshotWindow = null,
    window_count: ?*u8 = null,
    tab_placement: *TabPlacement,
    cwds: ?*[local.max_terminals]SnapshotCwd = null,
    references: ?*attachments.Table = null,
};

fn sinkFor(version: u16, out: *PersistedTopologySnapshot) Sink {
    switch (version) {
        2 => {
            out.* = .{ .v2 = .{} };
            return .{ .tabs = &out.v2.tabs, .tab_count = &out.v2.tab_count, .selection = &out.v2.selection, .tab_placement = &out.v2.tab_placement };
        },
        3 => {
            out.* = .{ .v3 = .{} };
            return .{ .tabs = &out.v3.tabs, .tab_count = &out.v3.tab_count, .selection = &out.v3.selection, .tab_placement = &out.v3.tab_placement, .cwds = &out.v3.cwds };
        },
        else => {
            const snapshot = if (version == 4) legacy: {
                out.* = .{ .v4 = .{ .version = 4 } };
                break :legacy &out.v4;
            } else current: {
                out.* = .{ .v5 = .{} };
                break :current &out.v5;
            };
            return .{ .tabs = &snapshot.tabs, .tab_count = &snapshot.tab_count, .tab_placement = &snapshot.tab_placement, .windows = &snapshot.windows, .window_count = &snapshot.window_count, .cwds = &snapshot.cwds, .references = if (version == 5) &snapshot.references else null };
        },
    }
}

const Parser = struct {
    sink: Sink,
    placement_seen: bool = false,
    selection_seen: bool = false,
    terminated: bool = false,
    tabs: usize = 0,
    windows: usize = 0,

    fn line(self: *Parser, text: []const u8) ParseError!void {
        if (text.len == 0) return;
        if (self.terminated) return error.InvalidState;
        var fields = std.mem.tokenizeScalar(u8, text, ' ');
        const Keyword = enum { end, placement, selection, window, tab, node, cwd, ref };
        const keyword = std.meta.stringToEnum(Keyword, fields.next() orelse return) orelse return error.InvalidState;
        try self.dispatch(keyword, text, &fields);
    }

    fn dispatch(self: *Parser, keyword: anytype, text: []const u8, fields: *Fields) ParseError!void {
        switch (keyword) {
            .end => {
                try finish(fields);
                self.terminated = true;
            },
            .placement => try self.placement(fields),
            .selection => try self.selected(fields),
            .window => try self.window(fields),
            .tab => try self.tab(fields),
            .node => try self.node(fields),
            .cwd => try self.cwd(text, fields),
            .ref => try self.reference(fields),
        }
    }

    fn placement(self: *Parser, fields: *Fields) ParseError!void {
        if (self.placement_seen) return error.InvalidState;
        self.placement_seen = true;
        self.sink.tab_placement.* = std.meta.stringToEnum(TabPlacement, try token(fields)) orelse return error.InvalidState;
        try finish(fields);
    }

    fn selected(self: *Parser, fields: *Fields) ParseError!void {
        const target = self.sink.selection orelse return error.InvalidState;
        if (self.selection_seen) return error.InvalidState;
        self.selection_seen = true;
        target.* = try selection(fields);
    }

    fn window(self: *Parser, fields: *Fields) ParseError!void {
        const table = self.sink.windows orelse return error.InvalidState;
        if (self.windows >= topology.max_snapshot_windows) return error.InvalidState;
        table[self.windows] = .{ .selection = try selection(fields) };
        self.windows += 1;
    }

    fn tab(self: *Parser, fields: *Fields) ParseError!void {
        if (self.tabs >= self.sink.tabs.len) return error.InvalidState;
        if (self.sink.windows != null and self.windows == 0) return error.InvalidState;
        self.sink.tabs[self.tabs] = .{ .root = try nodeId(fields), .focus = try nodeId(fields) };
        try finish(fields);
        self.tabs += 1;
        if (self.sink.windows) |table| {
            if (table[self.windows - 1].tab_count == topology.max_tabs) return error.InvalidState;
            table[self.windows - 1].tab_count += 1;
        }
    }

    fn node(self: *Parser, fields: *Fields) ParseError!void {
        if (self.tabs == 0) return error.InvalidState;
        const index = try number(usize, fields);
        if (index >= layout.max_nodes) return error.InvalidState;
        const target = &self.sink.tabs[self.tabs - 1].nodes[index];
        if (target.kind != .free) return error.InvalidState;
        const kind = try token(fields);
        const parent = try nodeId(fields);
        target.* = try self.nodeBody(kind, fields);
        target.parent = parent;
        try finish(fields);
    }

    fn nodeBody(self: *Parser, kind: []const u8, fields: *Fields) ParseError!topology.SnapshotNode {
        if (std.mem.eql(u8, kind, "leaf")) {
            return .{ .kind = .leaf, .terminal = @enumFromInt(local.first_terminal_raw + try localOffset(fields)), .has_terminal = true };
        }
        if (std.mem.eql(u8, kind, "remote")) {
            if (self.sink.references == null) return error.InvalidState;
            const index = try number(u8, fields);
            if (index >= attachments.max_references) return error.InvalidState;
            return .{ .kind = .leaf, .remote_ref = index, .has_terminal = true };
        }
        if (!std.mem.eql(u8, kind, "branch")) return error.InvalidState;
        return parseBranch(fields);
    }

    fn cwd(self: *Parser, text: []const u8, fields: *Fields) ParseError!void {
        const cwds = self.sink.cwds orelse return error.InvalidState;
        const offset_text = try token(fields);
        var offset_fields = std.mem.tokenizeScalar(u8, offset_text, ' ');
        const offset = try localOffset(&offset_fields);
        const consumed = @intFromPtr(offset_text.ptr) - @intFromPtr(text.ptr) + offset_text.len;
        if (consumed + 1 >= text.len) return error.InvalidState;
        cwds[offset].set(text[consumed + 1 ..]);
    }

    fn reference(self: *Parser, fields: *Fields) ParseError!void {
        const table = self.sink.references orelse return error.InvalidState;
        const index = try number(u8, fields);
        if (index != table.count) return error.InvalidState;
        const provider_id = try number(u64, fields);
        const kind = try number(u32, fields);
        const id = try number(u32, fields);
        var host: [255]u8 = undefined;
        const remote = contract.RemoteTerminalId.fromPhux(kind, id, try hex(fields, &host)) catch return error.InvalidState;
        const context = try parseContext(fields);
        _ = table.append(.{ .terminal_ref = .{ .provider_id = @enumFromInt(provider_id), .terminal_id = .{ .phux = remote } }, .context = context }) catch return error.InvalidState;
        try finish(fields);
    }
};

fn parseBranch(fields: *Fields) ParseError!topology.SnapshotNode {
    const orientation = std.meta.stringToEnum(layout.Orientation, try token(fields)) orelse return error.InvalidState;
    const fraction = std.fmt.parseFloat(f32, try token(fields)) catch return error.InvalidState;
    if (!std.math.isFinite(fraction)) return error.InvalidState;
    return .{ .kind = .branch, .orientation = orientation, .fraction = fraction, .first = try nodeId(fields), .second = try nodeId(fields) };
}

fn parseContext(fields: *Fields) ParseError!attachments.Context {
    var endpoint: [attachments.max_endpoint_bytes]u8 = undefined;
    var server_id: [attachments.max_server_id_bytes]u8 = undefined;
    return attachments.Context.init(try hex(fields, &endpoint), try hex(fields, &server_id), try number(u32, fields)) catch error.InvalidState;
}

fn headerVersion(header: []const u8) ParseError!u16 {
    var fields = std.mem.tokenizeScalar(u8, header, ' ');
    if (!std.mem.eql(u8, try token(&fields), magic)) return error.InvalidState;
    const version = try number(u16, &fields);
    try finish(&fields);
    if (version < min_readable_version or version > topology.topology_snapshot_version) return error.InvalidState;
    return version;
}

pub fn parse(bytes: []const u8, out: *PersistedTopologySnapshot) bool {
    if (bytes.len > max_state_bytes) return false;
    var lines = std.mem.splitScalar(u8, bytes, '\n');
    const header = std.mem.trimEnd(u8, lines.next() orelse return false, "\r");
    const version = headerVersion(header) catch return false;
    var parser: Parser = .{ .sink = sinkFor(version, out) };
    while (lines.next()) |line| parser.line(std.mem.trimEnd(u8, line, "\r")) catch return false;
    if (!parser.terminated) return false;
    parser.sink.tab_count.* = @intCast(parser.tabs);
    if (parser.sink.window_count) |count| count.* = @intCast(parser.windows);
    return true;
}
