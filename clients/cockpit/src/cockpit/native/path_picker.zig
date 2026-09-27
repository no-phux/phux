//! Insert Path: a separate host query, never Go to Directory's tab creator.
//! The picker retains its focused pane and provider lease; UI rows are display
//! only and an index is resolved against the correlated native result.
const std = @import("std");
const support = @import("../phux_support.zig");
const query = support.path_queries;
const directory = @import("directory_picker.zig");
const shell_words = @import("../shell_words.zig");
const projection = @import("workspace_projection.zig");

pub const request_name = "cockpit.path";
pub const max_bytes = directory.max_bytes;
pub const here_index: u16 = directory.here_index;
const max_path = directory.max_path_bytes;
pub const Kind = enum(u8) { open = 1, page = 2, search = 3, descend = 4, parent = 5, insert = 6 };
const Status = directory.Status;
pub const Error = error{ InvalidRequest, BufferTooSmall, StaleListing, Refused, Unavailable, Unsupported };

pub const Request = struct { kind: Kind, id: u32, offset: u16, index: u16, text: []const u8 };
pub fn decode(bytes: []const u8) Error!Request {
    if (bytes.len < 11 or bytes[0] != 1) return error.InvalidRequest;
    const kind = std.enums.fromInt(Kind, bytes[1]) orelse return error.InvalidRequest;
    if (bytes[10] > 64 or bytes.len != 11 + @as(usize, bytes[10])) return error.InvalidRequest;
    return .{ .kind = kind, .id = std.mem.readInt(u32, bytes[2..6], .little), .offset = std.mem.readInt(u16, bytes[6..8], .little), .index = std.mem.readInt(u16, bytes[8..10], .little), .text = bytes[11..] };
}

pub const Origin = struct {
    coordinator: support.ProviderId = .phux,
    terminal: ?support.TerminalRef = null,
    owner: ?support.ReplicaOwner = null,
    satellite: bool = false,

    fn host(self: *const Origin) []const u8 {
        const ref = self.terminal orelse return "";
        return if (self.satellite) ref.terminal_id.phux.host() else "";
    }
};

fn capture(engine: anytype) Error!void {
    const ref = engine.model.focusedTerminalRef() orelse return error.Unavailable;
    if (support.providerKind(ref) != .phux) return error.Unavailable;
    const remote = engine.model.phuxForRef(ref) orelse return error.Unavailable;
    if (remote.state() != .attached) return error.Unavailable;
    if (!query.supported(remote.host)) return error.Unsupported;
    const owner = engine.model.terminalOwner(ref) orelse return error.Unavailable;
    engine.path_origin = .{ .coordinator = ref.provider_id, .terminal = ref, .owner = owner, .satellite = ref.terminal_id.phux.host().len > 0 };
}

fn current(engine: anytype) Error!*support.PhuxProvider {
    const origin = &engine.path_origin;
    if (!targetIsCurrent(engine.model, origin)) return error.Unavailable;
    const remote = engine.model.phuxFor(origin.coordinator) orelse return error.Unavailable;
    if (remote.state() != .attached or !query.supported(remote.host)) return error.Unavailable;
    return remote;
}

fn targetIsCurrent(model: anytype, origin: *const Origin) bool {
    const ref = origin.terminal orelse return false;
    const owner = origin.owner orelse return false;
    const focused = model.focusedTerminalRef() orelse return false;
    if (!focused.eql(ref)) return false;
    const now = model.terminalOwner(ref) orelse return false;
    return now.eql(owner);
}

fn infoFor(remote: anytype, id: u32) Error!query.Info {
    const info = query.info(remote.host);
    if (info.request_id != id or id == 0) return error.StaleListing;
    return info;
}

fn startRoot(engine: anytype, remote: anytype) []const u8 {
    const ref = engine.path_origin.terminal orelse return "";
    for (remote.catalogTerminals()) |*entry| {
        if (!entry.terminal_ref.eql(ref)) continue;
        const cwd = entry.cwd.slice();
        if (cwd.len > 0 and cwd[0] == '/' and cwd.len <= max_path) return cwd;
    }
    return "";
}

fn requestOn(engine: anytype, remote: anytype, root: []const u8, text: []const u8, recursive: bool) Error!void {
    // The C adapter copies both strings before returning. No local fallback is
    // permitted for a named satellite, even if it is down or too old.
    _ = query.request(remote.host, root, text, recursive, engine.path_origin.host()) catch return error.Refused;
}

fn chooseRoot(remote: anytype, req: Request) Error![]const u8 {
    const info = try infoFor(remote, req.id);
    if (info.status != 2) return error.StaleListing;
    if (req.index == here_index) return info.root;
    const row = query.entry(remote.host, req.index) orelse return error.InvalidRequest;
    if (row.kind != 1) return error.InvalidRequest;
    return row.path;
}

fn insert(engine: anytype, remote: anytype, req: Request) Error!void {
    const path = try insertPath(remote, req);
    var quoted: [shell_words.max_quoted_bytes]u8 = undefined;
    const literal = try quotePath(path, &quoted);
    const owner = engine.path_origin.owner orelse return error.Unavailable;
    const ref = engine.path_origin.terminal orelse return error.Unavailable;
    // Resolve again immediately before input: the modal may have outlived a
    // lease, a pane replacement, or a focus switch while the query was pending.
    _ = try current(engine);
    if (support.providerKind(ref) != .phux) return error.Unavailable;
    remote.sendPaste(owner, literal, false) catch return error.Refused;
}

fn quotePath(path: []const u8, out: []u8) Error![]const u8 {
    if (path.len == 0 or path[0] != '/' or path.len > max_path) return error.Refused;
    const view = std.unicode.Utf8View.init(path) catch return error.Refused;
    var chars = view.iterator();
    while (chars.nextCodepoint()) |cp| {
        if (cp < 0x20 or (cp >= 0x7f and cp <= 0x9f)) return error.Refused;
    }
    const words = [_][]const u8{path};
    return shell_words.quotePaths(&words, out) orelse error.Refused;
}

fn insertPath(remote: anytype, req: Request) Error![]const u8 {
    const info = try infoFor(remote, req.id);
    if (info.status != 2) return error.StaleListing;
    if (req.index == here_index) return info.root;
    const row = query.entry(remote.host, req.index) orelse return error.InvalidRequest;
    return row.path;
}

fn requestFromListing(engine: anytype, remote: anytype, req: Request) Error!void {
    const info = try infoFor(remote, req.id);
    const path = switch (req.kind) {
        .search => info.root,
        .parent => info.parent orelse return error.InvalidRequest,
        .descend => try chooseRoot(remote, req),
        else => return error.InvalidRequest,
    };
    var root: [max_path]u8 = undefined;
    const copied = try copyRoot(path, &root);
    const searching = req.kind == .search and req.text.len > 0;
    try requestOn(engine, remote, copied, if (searching) req.text else "", searching);
}

fn copyRoot(path: []const u8, out: *[max_path]u8) Error![]const u8 {
    if (path.len > out.len) return error.Refused;
    @memcpy(out[0..path.len], path);
    return out[0..path.len];
}

/// The response deliberately shares the small directory-page envelope so the
/// same dialog can render either workflow. Rows carry only display labels;
/// the full path remains in the provider's retained result.
pub fn handle(engine: anytype, fx: anytype, payload: []const u8, out: []u8) Error![]const u8 {
    const req = try decode(payload);
    _ = fx;
    if (comptime !support.phux_enabled) return notice(req, "Phux is unavailable", out);
    if (req.kind == .open) capture(engine) catch |err| return notice(req, if (err == error.Unsupported)
        "Host path discovery unavailable; update phux on this host"
    else
        "Choose an attached Phux terminal to insert a path", out);
    const remote = current(engine) catch return notice(req, "The target pane or host changed. Reopen Insert Path.", out);
    apply(engine, remote, req) catch |err| {
        if (req.kind == .open) return notice(req, "Host path discovery unavailable; update phux on this host", out);
        return err;
    };
    return page(engine, remote, req, out);
}

fn apply(engine: anytype, remote: anytype, req: Request) Error!void {
    switch (req.kind) {
        .open => try requestOn(engine, remote, startRoot(engine, remote), "", false),
        .page => {},
        .search, .descend, .parent => try requestFromListing(engine, remote, req),
        .insert => try insert(engine, remote, req),
    }
}

fn writeRows(w: *Writer, host: anytype, info: query.Info, req: Request) Error!void {
    const count_at = w.at;
    try w.byte(0);
    if (info.status != 2) return;
    for (req.offset..@min(info.entry_count, @as(usize, req.offset) + 4)) |index| {
        const row = query.entry(host, index) orelse continue;
        if (!try writeRow(w, row, index, req.text.len > 0)) continue;
        w.out[count_at] += 1;
    }
}

fn writeRow(w: *Writer, row: query.Entry, index: usize, searching: bool) Error!bool {
    if (row.path.len == 0 or row.path[0] != '/' or index >= here_index) return false;
    try w.u16le(@intCast(index));
    try w.byte(@as(u8, @intFromBool(row.kind == 2)) | (@as(u8, @intFromBool(row.kind == 1)) << 1));
    try w.text(if (searching) row.path else std.fs.path.basenamePosix(row.path), 255);
    return true;
}

fn page(engine: anytype, remote: anytype, req: Request, out: []u8) Error![]const u8 {
    const info = query.info(remote.host);
    if (req.kind == .page and req.id != info.request_id) return error.StaleListing;
    var w = Writer{ .out = out };
    try w.header(wireStatus(info.status), info.request_id, resultFlags(info), info.entry_count, req.offset);
    try w.text(info.root, 240);
    try w.text(req.text, 64);
    try writeRows(&w, remote.host, info, req);
    try pageFooter(&w, engine, info);
    return out[0..w.at];
}

fn resultFlags(info: query.Info) u8 {
    const status: u8 = if (info.result_status == 2) 1 else if (info.result_status == 1) 4 else 0;
    return status | (if (info.parent != null) @as(u8, 8) else 0);
}

fn wireStatus(value: u32) Status {
    return switch (value) {
        1 => .pending,
        2 => .listed,
        3 => .refused,
        4 => .unknown_outcome,
        else => .unavailable,
    };
}

fn pageFooter(w: *Writer, engine: anytype, info: query.Info) Error!void {
    try w.text(info.message, 240);
    try w.byte(if (engine.path_origin.satellite) 1 else 0);
    try w.text(engine.path_origin.host(), 255);
    const via = if (engine.path_origin.coordinator == engine.model.attachmentAuthority()) "" else projection.peerHostLabel(engine.model, engine.path_origin.coordinator);
    try w.text(via, 255);
}

fn notice(req: Request, text: []const u8, out: []u8) Error![]const u8 {
    var w = Writer{ .out = out };
    try w.header(.unsupported, 0, 0, 0, req.offset);
    try w.text("", 240);
    try w.text(req.text, 64);
    try w.byte(0);
    try w.text(text, 240);
    try w.byte(0);
    try w.text("", 255);
    try w.text("", 255);
    return out[0..w.at];
}

const Writer = struct {
    out: []u8,
    at: usize = 0,
    fn byte(self: *Writer, value: u8) Error!void {
        if (self.at == self.out.len) return error.BufferTooSmall;
        self.out[self.at] = value;
        self.at += 1;
    }
    fn u16le(self: *Writer, value: u16) Error!void {
        try self.byte(@truncate(value));
        try self.byte(@truncate(value >> 8));
    }
    fn header(self: *Writer, status: Status, id: u32, flags: u8, count: usize, offset: u16) Error!void {
        try self.byte(1);
        try self.byte(@intFromEnum(status));
        var raw: [4]u8 = undefined;
        std.mem.writeInt(u32, &raw, id, .little);
        for (raw) |value| try self.byte(value);
        try self.byte(flags);
        try self.byte(0);
        try self.u16le(@intCast(@min(count, 65535)));
        try self.u16le(offset);
    }
    fn text(self: *Writer, value: []const u8, comptime bound: usize) Error!void {
        var buffer: [bound]u8 = undefined;
        const shown = @import("ts_navigation.zig").displayText(value, &buffer);
        try self.byte(@intCast(shown.len));
        if (self.at + shown.len > self.out.len) return error.BufferTooSmall;
        @memcpy(self.out[self.at..][0..shown.len], shown);
        self.at += shown.len;
    }
};

test "path requests reject unknown kinds and malformed lengths" {
    try std.testing.expectEqual(Kind.search, (try decode("\x01\x03\x07\x00\x00\x00\x00\x00\x00\x00\x03foo")).kind);
    try std.testing.expectError(error.InvalidRequest, decode("\x01\x07\x07\x00\x00\x00\x00\x00\x00\x00\x00"));
    try std.testing.expectError(error.InvalidRequest, decode("\x01\x03\x07\x00\x00\x00\x00\x00\x00\x00\x03fo"));
}

test "host path insertion refuses terminal controls and preserves shell literals" {
    var out: [shell_words.max_quoted_bytes]u8 = undefined;
    try std.testing.expectEqualStrings("'/a b/it'\\''s' ", try quotePath("/a b/it's", &out));
    try std.testing.expectEqualStrings("'/tmp/ café' ", try quotePath("/tmp/ café", &out));
    try std.testing.expectError(error.Refused, quotePath("/tmp/line\nnext", &out));
    try std.testing.expectError(error.Refused, quotePath("/tmp/\x1b[2J", &out));
    try std.testing.expectError(error.Refused, quotePath("/tmp/\xc2\x85", &out));
    try std.testing.expectError(error.Refused, quotePath("relative", &out));
}

test "satellite origin names the captured pane's host, never a default coordinator" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const ref: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(1, 9, "build-host") } };
    const routed: Origin = .{ .terminal = ref, .satellite = true };
    try std.testing.expectEqualStrings("build-host", routed.host());
    const serving: Origin = .{ .terminal = ref };
    try std.testing.expectEqualStrings("", serving.host());
}

test "insertion target refuses focus change and replacement lease" {
    const ref: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(1, 9, "") } };
    const other: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(1, 10, "") } };
    const owner: support.ReplicaOwner = .{ .terminal_ref = ref, .generation = .{ .epoch_id = 3, .stream_id = 4, .bootstrap_id = 5 }, .source_context = 8 };
    const origin: Origin = .{ .terminal = ref, .owner = owner };
    const Fake = struct {
        focused: support.TerminalRef,
        lease: support.ReplicaOwner,
        fn focusedTerminalRef(self: *@This()) ?support.TerminalRef {
            return self.focused;
        }
        fn terminalOwner(self: *@This(), _: support.TerminalRef) ?support.ReplicaOwner {
            return self.lease;
        }
    };
    var model: Fake = .{ .focused = ref, .lease = owner };
    try std.testing.expect(targetIsCurrent(&model, &origin));
    model.focused = other;
    try std.testing.expect(!targetIsCurrent(&model, &origin));
    model.focused = ref;
    model.lease.generation.epoch_id += 1;
    try std.testing.expect(!targetIsCurrent(&model, &origin));
    model.lease = owner;
    model.lease.source_context += 1;
    try std.testing.expect(!targetIsCurrent(&model, &origin));
}
