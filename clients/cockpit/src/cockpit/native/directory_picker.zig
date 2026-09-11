//! Go to Directory: the engine half of Cockpit's directory picker.
//!
//! One bounded request, `cockpit.directory`, carries an action and returns
//! a four-row page of the listing it leaves behind (docs/DIRECTORY_PICKER.md
//! has the tables):
//!
//!   request  version=1, kind:u8, request_id:u32, offset:u16, index:u16,
//!            query_len:u8, query UTF-8 (<= 64)
//!            kind 1 open, 2 page, 3 descend into entry `index`, 4 parent,
//!            5 open a new tab in the listed directory (`index` 0xffff) or
//!            in entry `index`
//!   reply    version=1, status:u8, request_id:u32, flags:u8 (bit 0
//!            truncated), error:u8, total:u16, offset:u16, path_len:u8,
//!            path, query_len:u8, query, count:u8, rows (index:u16,
//!            flags:u8 bit 0 symlink, name_len:u8, name), message_len:u8,
//!            message, scope:u8 (Scope), host_len:u8, host, via_len:u8, via
//!            (the coordinator the listing came through when it is not the
//!            active one; Open Here is unavailable then)
//!
//! The listing itself is the connected server's (LIST_DIRECTORY,
//! docs/spec/L3.md section 4), retained by the one Phux provider's client, so
//! it names directories on whichever host Cockpit is attached to: this Mac's
//! coordinator or a registered remote host. Opened over a satellite pane of a
//! hub that advertises LIST_DIRECTORY_HOST, it names that satellite instead
//! (section 4.1) and keeps naming it until the picker is opened again; a hub
//! without the bit lists itself, and the reply's scope says so. Paths never cross to TypeScript
//! as authority: the core names a row by its index in the listing and the
//! request ID that produced it, and the engine composes the path. Filtering
//! and paging happen here so a reply stays inside the toolkit's 4096-byte
//! completion, however large the directory.

const std = @import("std");
const support = @import("../phux_support.zig");
const model_module = @import("../model.zig");
const navigation = @import("ts_navigation.zig");
const projection = @import("workspace_projection.zig");

const Model = model_module.Model;

pub const request_name = "cockpit.directory";
pub const version: u8 = 1;
/// The switcher's row budget at the 420pt minimum window.
pub const page_size = 4;
pub const max_query_bytes: usize = 64;
/// Display bound for the path and the refusal message.
pub const max_text_bytes: usize = 240;
/// One path component on every filesystem Cockpit runs on.
pub const max_name_bytes: usize = 255;
/// LIST_DIRECTORY's request path bound.
pub const max_path_bytes: usize = 4096;
/// Synthetic rows, listed ahead of the entries while the filter is empty.
pub const here_index: u16 = 0xffff;
pub const up_index: u16 = 0xfffe;

pub const max_bytes: usize = 12 + 1 + max_text_bytes + 1 + max_query_bytes + 1 +
    page_size * (4 + max_name_bytes) + 1 + max_text_bytes + 2 + max_name_bytes + 1 + max_name_bytes;

comptime {
    std.debug.assert(max_bytes <= 4096);
}

pub const Kind = enum(u8) { open = 1, page = 2, descend = 3, parent = 4, here = 5 };
pub const Status = enum(u8) { unsupported = 0, pending = 1, listed = 2, refused = 3, unknown_outcome = 4, unavailable = 5 };
pub const Error = error{ InvalidRequest, BufferTooSmall, StaleListing, Refused, OtherCoordinator };

pub const Request = struct {
    kind: Kind,
    request_id: u32 = 0,
    offset: u16 = 0,
    index: u16 = 0,
    query: []const u8 = "",
};

pub fn decode(bytes: []const u8) Error!Request {
    if (bytes.len < 11 or bytes[0] != version) return error.InvalidRequest;
    const kind = std.enums.fromInt(Kind, bytes[1]) orelse return error.InvalidRequest;
    const query_len: usize = bytes[10];
    if (query_len > max_query_bytes or bytes.len != 11 + query_len) return error.InvalidRequest;
    return .{
        .kind = kind,
        .request_id = std.mem.readInt(u32, bytes[2..6], .little),
        .offset = std.mem.readInt(u16, bytes[6..8], .little),
        .index = std.mem.readInt(u16, bytes[8..10], .little),
        .query = bytes[11..],
    };
}

/// Apply one request on the owning thread and encode the page it leaves.
pub fn handle(engine: anytype, payload: []const u8, out: []u8) Error![]const u8 {
    const request = try decode(payload);
    if (comptime !support.phux_enabled)
        return encodeNotice(.unavailable, no_coordinator, request, out);
    if (request.kind == .open and !beginListing(engine)) return encodeNotice(.unavailable, no_coordinator, request, out);
    const remote = engine.model.phuxFor(engine.directory_origin.coordinator) orelse
        return encodeNotice(.unavailable, no_coordinator, request, out);
    if (listingNotice(remote)) |notice| return encodeNotice(notice.status, notice.message, request, out);
    switch (request.kind) {
        .open => try open(engine, remote),
        .page => {},
        .descend => try descend(engine, remote, request),
        .parent => try parent(engine, remote, request),
        .here => try openHere(engine, remote, request),
    }
    return encodePage(engine.model, remote, &engine.directory_origin, request, out);
}

const no_coordinator = "Go to Directory needs a Phux coordinator";

/// The listing belongs to the coordinator that minted the focused pane,
/// fixed when the picker opens: another coordinator is never asked, so a
/// satellite name is only ever resolved by the hub it came from.
fn beginListing(engine: anytype) bool {
    const coordinator = focusedCoordinator(engine.model) orelse return false;
    engine.directory_origin = .{ .coordinator = coordinator };
    return true;
}

/// Why this coordinator cannot list right now, if it cannot.
fn listingNotice(remote: anytype) ?struct { status: Status, message: []const u8 } {
    if (remote.state() != .attached) return .{ .status = .unavailable, .message = "Phux is not connected. Reconnect, then try again." };
    if (!remote.directorySupported()) return .{ .status = .unsupported, .message = "This coordinator cannot list directories. Update phux on that host." };
    return null;
}

/// The focused pane's coordinator, else the active one.
fn focusedCoordinator(model: *Model) ?support.ProviderId {
    if (model.focusedTerminalRef()) |ref| {
        if (support.providerKind(ref) == .phux and model.phuxForRef(ref) != null) return ref.provider_id;
    }
    const active = model.phux() orelse return null;
    return active.providerId();
}

/// The coordinator a listing came through, when it is not the active one:
/// its tabs cannot be opened here, and the heading names it.
fn viaLabel(model: *const Model, origin: *const Origin) []const u8 {
    if (origin.coordinator == model.attachmentAuthority()) return "";
    return projection.peerHostLabel(model, origin.coordinator);
}

/// Whose directories the retained listing names (reply trailer `scope`).
pub const Scope = enum(u8) {
    /// The connected coordinator's own host.
    coordinator = 0,
    /// A satellite of the connected hub, named by the pane the picker was
    /// opened over, relayed by the hub (LIST_DIRECTORY.host, L3 4.1).
    satellite = 1,
    /// The coordinator's own host, listed in place of the focused
    /// satellite's because the hub does not advertise LIST_DIRECTORY_HOST
    /// (it would ignore the field and list itself). Said, never implied.
    coordinator_instead = 2,
};

/// The host a listing reads, fixed when the picker opens and kept for every
/// descend, parent and Open Here that follows, as the TUI keeps `host` on
/// every row. `terminal` is the satellite pane it was opened over; it owns
/// its host storage, so the host never borrows client or catalog memory.
pub const Origin = struct {
    /// The coordinator whose provider asks LIST_DIRECTORY for this listing.
    coordinator: support.ProviderId = .phux,
    scope: Scope = .coordinator,
    terminal: ?support.TerminalRef = null,

    /// The satellite LIST_DIRECTORY names, or empty for the serving host.
    pub fn listedHost(origin: *const Origin) []const u8 {
        if (origin.scope != .satellite) return "";
        return origin.namedHost();
    }

    /// The satellite the picker was opened over, whether or not it is the
    /// one being listed.
    pub fn namedHost(origin: *const Origin) []const u8 {
        const ref = if (origin.terminal) |*value| value else return "";
        return ref.terminal_id.phux.host();
    }
};

/// The focused pane decides the host. A satellite-tagged Phux pane lists its
/// satellite when the hub can relay the request, and otherwise the
/// coordinator, labeled as such. Anything else lists the coordinator.
pub fn originFor(model: *const Model, remote: anytype) Origin {
    const coordinator = remote.providerId();
    const ref = model.focusedTerminalRef() orelse return .{ .coordinator = coordinator };
    // Only a pane this coordinator minted names one of its satellites.
    if (support.providerKind(ref) != .phux or ref.provider_id != coordinator) return .{ .coordinator = coordinator };
    if (ref.terminal_id.phux.host().len == 0) return .{ .coordinator = coordinator };
    const scope: Scope = if (remote.directoryHostSupported()) .satellite else .coordinator_instead;
    return .{ .coordinator = coordinator, .scope = scope, .terminal = ref };
}

/// Start where the focused terminal is, when the listed host is the one it
/// runs on; otherwise that host's serving user's home. A local PTY's
/// directory is on this Mac, and a satellite's is not on its hub.
fn open(engine: anytype, remote: anytype) Error!void {
    engine.directory_origin = originFor(engine.model, remote);
    var buffer: [max_path_bytes]u8 = undefined;
    const start = startDirectory(engine.model, remote, &engine.directory_origin, &buffer);
    _ = remote.requestDirectoryOn(start, engine.directory_origin.listedHost()) catch return error.Refused;
}

/// The terminal whose directory the listing starts in, if it runs on the
/// listed host.
fn startTerminal(model: *const Model, origin: *const Origin) ?support.TerminalRef {
    switch (origin.scope) {
        .satellite => return origin.terminal,
        .coordinator_instead => return null,
        .coordinator => {
            const ref = model.focusedTerminalRef() orelse return null;
            if (support.providerKind(ref) != .phux) return null;
            if (ref.terminal_id.phux.host().len != 0) return null;
            return ref;
        },
    }
}

fn startDirectory(model: *const Model, remote: anytype, origin: *const Origin, out: *[max_path_bytes]u8) []const u8 {
    const ref = startTerminal(model, origin) orelse return "";
    for (remote.catalogTerminals()) |*entry| {
        if (!entry.terminal_ref.eql(ref)) continue;
        const cwd = entry.cwd.slice();
        if (cwd.len == 0 or cwd[0] != '/' or cwd.len > out.len) return "";
        @memcpy(out[0..cwd.len], cwd);
        return out[0..cwd.len];
    }
    return "";
}

/// The retained listing, when `expected` is still the one it answers. An
/// action on anything older is refused rather than applied to a directory
/// the user was not looking at.
fn currentInfo(remote: anytype, expected: u32) Error!@TypeOf(remote.directoryInfo()) {
    const info = remote.directoryInfo();
    if (info.request_id != expected) return error.StaleListing;
    return info;
}

fn descend(engine: anytype, remote: anytype, request: Request) Error!void {
    const info = try currentInfo(remote, request.request_id);
    if (info.status != .listed) return error.StaleListing;
    var buffer: [max_path_bytes]u8 = undefined;
    const path = try entryPath(remote, info.path, request.index, &buffer);
    _ = remote.requestDirectoryOn(path, engine.directory_origin.listedHost()) catch return error.Refused;
}

fn parent(engine: anytype, remote: anytype, request: Request) Error!void {
    const info = try currentInfo(remote, request.request_id);
    const up = parentOf(info) orelse return error.InvalidRequest;
    var buffer: [max_path_bytes]u8 = undefined;
    const path = copyPath(up, &buffer) orelse return error.Refused;
    _ = remote.requestDirectoryOn(path, engine.directory_origin.listedHost()) catch return error.Refused;
}

/// A satellite listing's tab opens on that satellite: the pane the picker
/// was opened over owns the spawn, so the provider routes it through the
/// hub's satellite relay (SPAWN_RESOURCE.satellite). A coordinator listing's
/// tab has no owner, so no focused satellite route can carry its directory
/// to another host.
fn openHere(engine: anytype, remote: anytype, request: Request) Error!void {
    // New tabs open on the active coordinator only; another coordinator's
    // directory would name a path on the wrong machine.
    if (viaLabel(engine.model, &engine.directory_origin).len != 0) return error.OtherCoordinator;
    const info = try currentInfo(remote, request.request_id);
    if (info.status != .listed) return error.StaleListing;
    var buffer: [max_path_bytes]u8 = undefined;
    const path = if (request.index == here_index)
        copyPath(info.path, &buffer) orelse return error.Refused
    else
        try entryPath(remote, info.path, request.index, &buffer);
    const owner = if (engine.directory_origin.scope == .satellite) engine.directory_origin.terminal else null;
    if (!engine.openTabAt(path, owner)) return error.Refused;
}

/// Listed: the server's lexical parent. Refused: the attempted path's, so a
/// directory the user may not read is still a place to go back up from.
fn parentOf(info: anytype) ?[]const u8 {
    if (info.parent) |value| return value;
    if (info.status != .refused or info.path.len == 0) return null;
    return std.fs.path.dirnamePosix(info.path);
}

/// Copied before any mutable provider call: the listing's spans are borrowed
/// from the client and a request replaces them.
fn copyPath(path: []const u8, out: *[max_path_bytes]u8) ?[]const u8 {
    if (path.len == 0 or path.len > out.len) return null;
    @memcpy(out[0..path.len], path);
    return out[0..path.len];
}

fn entryPath(remote: anytype, directory: []const u8, index: u16, out: *[max_path_bytes]u8) Error![]const u8 {
    const entry = remote.directoryEntry(index) orelse return error.InvalidRequest;
    return childPath(directory, entry.name, out) orelse error.Refused;
}

pub fn childPath(directory: []const u8, name: []const u8, out: []u8) ?[]const u8 {
    if (directory.len == 0 or name.len == 0) return null;
    const separator: []const u8 = if (std.mem.endsWith(u8, directory, "/")) "" else "/";
    return std.fmt.bufPrint(out, "{s}{s}{s}", .{ directory, separator, name }) catch null;
}

const Row = struct { index: u16, symlink: bool = false, name: []const u8 = "" };

const Page = struct {
    rows: [page_size]Row = undefined,
    count: usize = 0,
    total: usize = 0,

    fn add(page: *Page, row: Row, offset: usize) void {
        if (page.total >= offset and page.count < page_size) {
            page.rows[page.count] = row;
            page.count += 1;
        }
        page.total += 1;
    }
};

fn collect(remote: anytype, info: anytype, query: []const u8, offset: usize, open_here: bool) Page {
    var page: Page = .{};
    if (query.len == 0) {
        if (open_here and info.status == .listed) page.add(.{ .index = here_index }, offset);
        if (parentOf(info) != null) page.add(.{ .index = up_index }, offset);
    }
    if (info.status != .listed) return page;
    for (0..info.entry_count) |index| {
        const entry = remote.directoryEntry(index) orelse continue;
        if (!projection.containsIgnoreCase(entry.name, query)) continue;
        page.add(.{ .index = @intCast(index), .symlink = entry.symlink, .name = entry.name }, offset);
    }
    return page;
}

fn wireStatus(status: anytype) Status {
    return switch (status) {
        .pending => .pending,
        .listed => .listed,
        .refused => .refused,
        .unknown_outcome => .unknown_outcome,
        else => .unavailable,
    };
}

fn encodePage(model: *const Model, remote: anytype, origin: *const Origin, request: Request, out: []u8) Error![]const u8 {
    const info = remote.directoryInfo();
    const via = viaLabel(model, origin);
    const page = collect(remote, info, request.query, request.offset, via.len == 0);
    var writer: Writer = .{ .out = out };
    try writer.header(wireStatus(info.status), info.request_id, info.truncated, @intCast(@min(info.error_code, 255)), page.total, request.offset);
    try writer.text(info.path, max_text_bytes);
    try writer.text(request.query, max_query_bytes);
    try writer.byte(@intCast(page.count));
    for (page.rows[0..page.count]) |row| {
        try writer.u16le(row.index);
        try writer.byte(@intFromBool(row.symlink));
        try writer.text(row.name, max_name_bytes);
    }
    try writer.text(info.message, max_text_bytes);
    try writer.byte(@intFromEnum(origin.scope));
    try writer.text(origin.namedHost(), max_name_bytes);
    try writer.text(via, max_name_bytes);
    return out[0..writer.at];
}

fn encodeNotice(status: Status, message: []const u8, request: Request, out: []u8) Error![]const u8 {
    var writer: Writer = .{ .out = out };
    try writer.header(status, 0, false, 0, 0, request.offset);
    try writer.text("", max_text_bytes);
    try writer.text(request.query, max_query_bytes);
    try writer.byte(0);
    try writer.text(message, max_text_bytes);
    try writer.byte(@intFromEnum(Scope.coordinator));
    try writer.text("", max_name_bytes);
    try writer.text("", max_name_bytes);
    return out[0..writer.at];
}

const Writer = struct {
    out: []u8,
    at: usize = 0,

    fn byte(self: *Writer, value: u8) Error!void {
        if (self.at >= self.out.len) return error.BufferTooSmall;
        self.out[self.at] = value;
        self.at += 1;
    }

    fn u16le(self: *Writer, value: u16) Error!void {
        try self.byte(@intCast(value & 0xff));
        try self.byte(@intCast(value >> 8));
    }

    fn header(self: *Writer, status: Status, request_id: u32, truncated: bool, error_code: u8, total: usize, offset: u16) Error!void {
        try self.byte(version);
        try self.byte(@intFromEnum(status));
        var id: [4]u8 = undefined;
        std.mem.writeInt(u32, &id, request_id, .little);
        for (id) |value| try self.byte(value);
        try self.byte(@intFromBool(truncated));
        try self.byte(error_code);
        try self.u16le(@intCast(@min(total, std.math.maxInt(u16))));
        try self.u16le(offset);
    }

    /// Length-prefixed and elided on a UTF-8 boundary; never refused.
    fn text(self: *Writer, value: []const u8, comptime bound: usize) Error!void {
        var buffer: [bound]u8 = undefined;
        const shown = navigation.displayText(value, &buffer);
        try self.byte(@intCast(shown.len));
        if (self.at + shown.len > self.out.len) return error.BufferTooSmall;
        @memcpy(self.out[self.at..][0..shown.len], shown);
        self.at += shown.len;
    }
};

test "requests are exact about their framing" {
    const page = try decode("\x01\x02\x07\x00\x00\x00\x04\x00\x00\x00\x03cfg");
    try std.testing.expectEqual(Kind.page, page.kind);
    try std.testing.expectEqual(@as(u32, 7), page.request_id);
    try std.testing.expectEqual(@as(u16, 4), page.offset);
    try std.testing.expectEqualStrings("cfg", page.query);
    const here = try decode("\x01\x05\x01\x00\x00\x00\x00\x00\xff\xff\x00");
    try std.testing.expectEqual(here_index, here.index);
    for ([_][]const u8{
        "",
        "\x02\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00",
        "\x01\x09\x00\x00\x00\x00\x00\x00\x00\x00\x00",
        "\x01\x02\x00\x00\x00\x00\x00\x00\x00\x00\x02a",
    }) |bytes| try std.testing.expectError(error.InvalidRequest, decode(bytes));
}

test "child paths join exactly one separator and refuse what cannot fit" {
    var out: [max_path_bytes]u8 = undefined;
    try std.testing.expectEqualStrings("/work/cockpit", childPath("/work", "cockpit", &out).?);
    try std.testing.expectEqualStrings("/etc", childPath("/", "etc", &out).?);
    try std.testing.expect(childPath("", "x", &out) == null);
    var long: [max_path_bytes]u8 = undefined;
    @memset(&long, 'a');
    try std.testing.expect(childPath(&long, "x", &out) == null);
}
