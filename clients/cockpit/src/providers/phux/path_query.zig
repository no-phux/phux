//! Owning-thread adapter for the additive host PATH_QUERY ABI.
//! Results are copied during the provider drain: C's borrowed answer batch is
//! invalidated by subsequent mutable client calls in that same drain.
const std = @import("std");
const c = @import("abi.zig").c;

pub const available = @hasDecl(c, "PhuxPathQuery") and @hasDecl(c, "PhuxPathAnswer") and
    @hasDecl(c, "PhuxPathRow") and @hasDecl(c, "phux_client_path_query_supported") and
    @hasDecl(c, "phux_client_path_query") and @hasDecl(c, "phux_client_path_answers_take") and
    @hasDecl(c, "phux_client_path_answer_get") and @hasDecl(c, "phux_client_path_row_get");

pub const Info = struct {
    status: u32 = 0, // none=0, pending=1, listed=2, refused=3, unknown=4
    request_id: u32 = 0,
    result_status: u32 = 0,
    entry_count: usize = 0,
    root: []const u8 = "",
    parent: ?[]const u8 = null,
    message: []const u8 = "",
};
pub const Entry = struct { path: []const u8, kind: u32 };

pub const State = struct {
    arena: ?std.heap.ArenaAllocator = null,
    info: Info = .{},
    entries: []const Entry = &.{},

    pub fn deinit(self: *State) void {
        if (self.arena) |*arena| arena.deinit();
        self.* = .{};
    }

    fn start(self: *State, arena: std.heap.ArenaAllocator, id: u32, root: []const u8) void {
        self.deinit();
        self.* = .{ .arena = arena, .info = .{ .status = 1, .request_id = id, .root = root } };
    }

    fn capture(self: *State, gpa: std.mem.Allocator, client: *c.PhuxClient, index: usize) !void {
        if (comptime available) {
            var raw = std.mem.zeroes(c.PhuxPathAnswer);
            raw.size = @sizeOf(c.PhuxPathAnswer);
            raw.version = c.PHUX_CLIENT_ABI_VERSION;
            if (c.phux_client_path_answer_get(client, index, &raw) != c.PHUX_CLIENT_OK) return;
            if (raw.request_id != self.info.request_id) return;
            var arena = std.heap.ArenaAllocator.init(gpa);
            errdefer arena.deinit();
            const alloc = arena.allocator();
            const root = try alloc.dupe(u8, slice(raw.root));
            const parent = if (raw.has_parent) try alloc.dupe(u8, slice(raw.parent)) else null;
            const message = try alloc.dupe(u8, slice(raw.message));
            const rows = try copyRows(alloc, client, index, if (raw.has_error) 0 else @min(raw.row_count, 1024));
            const status: u32 = answerStatus(raw.has_error, raw.@"error");
            self.deinit();
            self.* = .{ .arena = arena, .entries = rows, .info = .{ .status = status, .request_id = raw.request_id, .root = root, .parent = parent, .message = message, .result_status = raw.status, .entry_count = rows.len } };
        }
    }
};

fn answerStatus(has_error: bool, code: u32) u32 {
    if (!has_error) return 2;
    if (comptime available) {
        if (code == c.PHUX_PATH_UNANSWERED) return 4;
    }
    return 3;
}

fn copyRows(alloc: std.mem.Allocator, client: *c.PhuxClient, answer_index: usize, count: usize) ![]const Entry {
    if (comptime available) {
        const rows = try alloc.alloc(Entry, count);
        for (rows, 0..) |*item, index| {
            var row = std.mem.zeroes(c.PhuxPathRow);
            row.size = @sizeOf(c.PhuxPathRow);
            row.version = c.PHUX_CLIENT_ABI_VERSION;
            if (c.phux_client_path_row_get(client, answer_index, index, &row) != c.PHUX_CLIENT_OK) return error.InvalidAnswer;
            const path = slice(row.path);
            if (path.len == 0 or path[0] != '/' or row.kind > 2) return error.InvalidAnswer;
            item.* = .{ .path = try alloc.dupe(u8, path), .kind = row.kind };
        }
        return rows;
    }
    return &.{};
}

fn bytes(value: []const u8) c.PhuxBytes {
    return .{ .data = if (value.len == 0) null else value.ptr, .len = value.len };
}

fn slice(raw: c.PhuxBytes) []const u8 {
    if (raw.data == null) return "";
    return raw.data[0..raw.len];
}

pub fn supported(host: anytype) bool {
    if (comptime available) {
        var value = false;
        return c.phux_client_path_query_supported(host.client, &value) == c.PHUX_CLIENT_OK and value;
    }
    return false;
}

pub fn request(host: anytype, root: []const u8, text: []const u8, recursive: bool, satellite: []const u8) !u32 {
    if (comptime available) {
        if (host.state() != .attached or !supported(host)) return error.Unavailable;
        const id = try host.operation_ledger.nextRequestId();
        var arena = std.heap.ArenaAllocator.init(host.gpa);
        errdefer arena.deinit();
        const copied_root = try arena.allocator().dupe(u8, root);
        const raw: c.PhuxPathQuery = .{ .size = @sizeOf(c.PhuxPathQuery), .version = c.PHUX_CLIENT_ABI_VERSION, .request_id = id, .root = bytes(root), .query = bytes(text), .recursive = recursive, .host = bytes(satellite) };
        var sent: u32 = 0;
        if (c.phux_client_path_query(host.client, &raw, &sent) != c.PHUX_CLIENT_OK or sent != id) return error.Refused;
        host.operation_ledger.last_id = id;
        host.path_results.start(arena, id, copied_root);
        host.stageOutgoing() catch host.disconnect();
        return id;
    }
    return error.Unavailable;
}

pub fn drain(host: anytype) !bool {
    if (comptime available) {
        if (!supported(host)) return false;
        var count: usize = 0;
        if (c.phux_client_path_answers_take(host.client, &count) != c.PHUX_CLIENT_OK) return error.InvalidAnswer;
        for (0..count) |index| try host.path_results.capture(host.gpa, host.client, index);
        return count > 0;
    }
    return false;
}

pub fn info(host: anytype) Info {
    return host.path_results.info;
}

pub fn entry(host: anytype, index: usize) ?Entry {
    if (index >= host.path_results.entries.len) return null;
    return host.path_results.entries[index];
}
