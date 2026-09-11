//! The remote hosts Cockpit remembers across launches.
//!
//! Phux-backed layout lives in each coordinator's shared workspace, not in
//! Cockpit's state file, so the client-side facts a relaunch needs are WHICH
//! coordinators to reattach to and, for each, which session it was showing
//! (ADR-0110). Each registered host Connect to Host attaches joins a short
//! list beside the workspace state file; Disconnect removes that host, and
//! its record with it, and Disconnect All removes the file. At startup every
//! remembered host is reattached beside this Mac, listing: none is attached
//! until one of its sessions is shown, and only a host whose tab was in
//! front is shown again (cockpit/native/peer_restore.zig).
//!
//! Three formats. v1, what earlier releases wrote, is a fixed header plus
//! exactly one `target=` line. v2 is a fixed header plus one to `max_hosts`
//! distinct `target=` lines, oldest first. v3 is v2 where at least one
//! `target=` line is followed by one `shown=` record; it is written only when
//! a record exists, so a file without one stays readable by v2 releases.
//! A file naming more than one front record keeps the first and reads the
//! others as not front. The file is written only when its bytes change, and
//! then atomically (a temporary file, synced, replaces it), so a torn write
//! cannot occur. Anything else (a hand edit, an unknown format) is treated as
//! absent: the cost of forgetting is one Connect to Host, and the cost of
//! misreading is dialing a host nobody chose, or showing a session nobody
//! had on screen.

const std = @import("std");
const config_module = @import("../config/config.zig");

pub const suffix = ".remote";
pub const header = "phux-cockpit-remote v1\n";
pub const list_header = "phux-cockpit-remote v2\n";
pub const shown_header = "phux-cockpit-remote v3\n";
const key = "target=";
const shown_key = "shown=";
/// This Mac plus three hosts is Cockpit's coordinator bound.
pub const max_hosts: usize = 3;
pub const max_file_bytes = header.len + key.len + config_module.max_phux_remote_bytes + 1;
/// `shown=` session `,` server hash `,` window id or `-` `,` front, newline.
const max_shown_line_bytes = shown_key.len + 10 + 1 + 16 + 1 + 32 + 1 + 1 + 1;
pub const max_list_file_bytes = shown_header.len + max_hosts * (key.len + config_module.max_phux_remote_bytes + 1 + max_shown_line_bytes);

/// What a remembered host's coordinator was showing (ADR-0110): the session,
/// the server incarnation it belongs to, the shared window of its selected
/// tab, and whether that tab was the selected tab of the front window.
/// Keyed by the `target=` line it follows, so by that host's coordinator id;
/// a session id is never read without it.
pub const Shown = struct {
    session: u32,
    /// `serverHash` of the server's HELLO_OK.server_id.
    server: u64,
    window: ?[16]u8 = null,
    front: bool = false,

    pub fn eql(a: Shown, b: Shown) bool {
        if (a.session != b.session or a.server != b.server or a.front != b.front) return false;
        const left = a.window orelse return b.window == null;
        const right = b.window orelse return false;
        return std.mem.eql(u8, &left, &right);
    }
};

/// The server incarnation a record belongs to. HELLO_OK.server_id changes on
/// every server exec, so a record of an earlier server never matches.
pub fn serverHash(server_id: []const u8) u64 {
    return std.hash.Wyhash.hash(0x7068_7578, server_id);
}

/// Remembered hosts, oldest first, each a valid `phux-remote` value, each
/// with at most one record of what it showed.
pub const Hosts = struct {
    storage: [max_hosts][config_module.max_phux_remote_bytes]u8 = undefined,
    lens: [max_hosts]usize = @splat(0),
    shown: [max_hosts]?Shown = @splat(null),
    count: usize = 0,

    pub fn get(self: *const Hosts, index: usize) []const u8 {
        return self.storage[index][0..self.lens[index]];
    }

    pub fn contains(self: *const Hosts, target: []const u8) bool {
        return self.indexOf(target) != null;
    }

    pub fn indexOf(self: *const Hosts, target: []const u8) ?usize {
        for (0..self.count) |index| {
            if (std.mem.eql(u8, self.get(index), target)) return index;
        }
        return null;
    }

    /// False when `target` is invalid, already listed, or the list is full.
    pub fn add(self: *Hosts, target: []const u8) bool {
        if (target.len == 0 or target.len > config_module.max_phux_remote_bytes) return false;
        if (!config_module.validPhuxRemote(target)) return false;
        if (self.contains(target) or self.count == max_hosts) return false;
        @memcpy(self.storage[self.count][0..target.len], target);
        self.lens[self.count] = target.len;
        self.shown[self.count] = null;
        self.count += 1;
        return true;
    }

    /// False when `target` was not listed. Its record goes with it; the
    /// others keep their order and their records.
    pub fn remove(self: *Hosts, target: []const u8) bool {
        const index = self.indexOf(target) orelse return false;
        for (index + 1..self.count) |next| {
            self.storage[next - 1] = self.storage[next];
            self.lens[next - 1] = self.lens[next];
            self.shown[next - 1] = self.shown[next];
        }
        self.count -= 1;
        self.shown[self.count] = null;
        return true;
    }

    /// Set the record of the host at `index`; true when it changed.
    pub fn setShown(self: *Hosts, index: usize, value: ?Shown) bool {
        const current = self.shown[index];
        const same = if (current) |old| (if (value) |new| old.eql(new) else false) else value == null;
        if (same) return false;
        self.shown[index] = value;
        return true;
    }

    fn hasShown(self: *const Hosts) bool {
        for (self.shown[0..self.count]) |value| if (value != null) return true;
        return false;
    }
};

/// The remembered hosts, from any format, into `out`: false, with `out`
/// empty, for anything malformed.
pub fn parseAll(bytes: []const u8, out: *Hosts) bool {
    out.* = .{};
    if (parse(bytes)) |target| return out.add(target);
    const records = std.mem.startsWith(u8, bytes, shown_header);
    if (!records and !std.mem.startsWith(u8, bytes, list_header)) return false;
    var rest = bytes[list_header.len..];
    var front_seen = false;
    while (rest.len != 0) {
        const line_end = std.mem.indexOfScalar(u8, rest, '\n') orelse return reject(out);
        const line = rest[0..line_end];
        rest = rest[line_end + 1 ..];
        if (std.mem.startsWith(u8, line, key)) {
            if (!out.add(line[key.len..])) return reject(out);
            continue;
        }
        // A record only in v3, only directly after its host, at most one each.
        if (!records or out.count == 0 or out.shown[out.count - 1] != null) return reject(out);
        if (!std.mem.startsWith(u8, line, shown_key)) return reject(out);
        var value = parseShown(line[shown_key.len..]) orelse return reject(out);
        // At most one tab is the front window's selected tab. A file that
        // names more keeps the first and reads the rest as not front, rather
        // than forgetting every host.
        if (value.front and front_seen) value.front = false;
        if (value.front) front_seen = true;
        out.shown[out.count - 1] = value;
    }
    // A v3 file always carries a record, since one without is written as v2.
    if (records and !out.hasShown()) return reject(out);
    return out.count != 0 or reject(out);
}

fn parseShown(text: []const u8) ?Shown {
    var fields = std.mem.splitScalar(u8, text, ',');
    const session_text = fields.next() orelse return null;
    const server_text = fields.next() orelse return null;
    const window_text = fields.next() orelse return null;
    const front_text = fields.next() orelse return null;
    if (fields.next() != null) return null;
    if (session_text.len == 0 or session_text.len > 10 or session_text[0] == '0') return null;
    for (session_text) |byte| if (!std.ascii.isDigit(byte)) return null;
    const session = std.fmt.parseUnsigned(u32, session_text, 10) catch return null;
    if (server_text.len != 16) return null;
    for (server_text) |byte| if (!std.ascii.isHex(byte)) return null;
    const server = std.fmt.parseUnsigned(u64, server_text, 16) catch return null;
    var window: ?[16]u8 = null;
    if (!std.mem.eql(u8, window_text, "-")) {
        if (window_text.len != 32) return null;
        var id: [16]u8 = undefined;
        _ = std.fmt.hexToBytes(&id, window_text) catch return null;
        window = id;
    }
    const front = if (std.mem.eql(u8, front_text, "1")) true else if (std.mem.eql(u8, front_text, "0")) false else return null;
    return .{ .session = session, .server = server, .window = window, .front = front };
}

fn reject(out: *Hosts) bool {
    out.* = .{};
    return false;
}

/// The file for `hosts` (v2 without a record, v3 with one), or null when
/// there is nothing to remember.
pub fn encodeAll(hosts: *const Hosts, out: []u8) ?[]const u8 {
    if (hosts.count == 0) return null;
    var at = append(out, 0, if (hosts.hasShown()) shown_header else list_header) orelse return null;
    for (0..hosts.count) |index| {
        at = append(out, at, key) orelse return null;
        at = append(out, at, hosts.get(index)) orelse return null;
        at = append(out, at, "\n") orelse return null;
        const value = hosts.shown[index] orelse continue;
        var line: [max_shown_line_bytes]u8 = undefined;
        at = append(out, at, encodeShown(value, &line) orelse return null) orelse return null;
    }
    return out[0..at];
}

fn encodeShown(value: Shown, out: *[max_shown_line_bytes]u8) ?[]const u8 {
    if (value.session == 0) return null;
    var window_text: [32]u8 = undefined;
    const window: []const u8 = if (value.window) |id| blk: {
        window_text = std.fmt.bytesToHex(id, .lower);
        break :blk &window_text;
    } else "-";
    return std.fmt.bufPrint(out, shown_key ++ "{d},{x:0>16},{s},{d}\n", .{ value.session, value.server, window, @intFromBool(value.front) }) catch null;
}

fn append(out: []u8, at: usize, text: []const u8) ?usize {
    if (at + text.len > out.len) return null;
    @memcpy(out[at..][0..text.len], text);
    return at + text.len;
}

/// Every remembered host into `out`; empty when there is none.
pub fn loadAll(io: std.Io, file_path: []const u8, out: *Hosts) void {
    out.* = .{};
    var file = std.Io.Dir.cwd().openFile(io, file_path, .{}) catch return;
    defer file.close(io);
    var bytes: [max_list_file_bytes + 1]u8 = undefined;
    const read = file.readPositionalAll(io, &bytes, 0) catch return;
    _ = parseAll(bytes[0..read], out);
}

/// Remember exactly `hosts`, or forget every host when it is empty. Best
/// effort, as `store`.
pub fn storeAll(io: std.Io, file_path: []const u8, hosts: *const Hosts) void {
    _ = save(io, file_path, hosts);
}

/// `storeAll`, saying whether the file changed. Bytes equal to the file's are
/// not written again, so a tab switch that moves no record writes nothing;
/// a write goes to a temporary file in the same directory, is synced, and
/// then replaces the file whole, so a torn write never forgets every host.
pub fn save(io: std.Io, file_path: []const u8, hosts: *const Hosts) bool {
    const cwd = std.Io.Dir.cwd();
    var bytes: [max_list_file_bytes]u8 = undefined;
    const encoded = encodeAll(hosts, &bytes) orelse {
        cwd.deleteFile(io, file_path) catch return false;
        return true;
    };
    if (fileHolds(io, file_path, encoded)) return false;
    if (std.fs.path.dirname(file_path)) |dir| cwd.createDirPath(io, dir) catch {};
    writeAtomic(io, file_path, encoded) catch return false;
    return true;
}

fn fileHolds(io: std.Io, file_path: []const u8, expected: []const u8) bool {
    var file = std.Io.Dir.cwd().openFile(io, file_path, .{}) catch return false;
    defer file.close(io);
    var current: [max_list_file_bytes + 1]u8 = undefined;
    const read = file.readPositionalAll(io, &current, 0) catch return false;
    return std.mem.eql(u8, current[0..read], expected);
}

fn writeAtomic(io: std.Io, file_path: []const u8, bytes: []const u8) !void {
    var atomic = try std.Io.Dir.cwd().createFileAtomic(io, file_path, .{ .replace = true });
    defer atomic.deinit(io);
    try atomic.file.writePositionalAll(io, bytes, 0);
    try atomic.file.sync(io);
    try atomic.replace(io);
}

var path_buffer: [std.fs.max_path_bytes]u8 = undefined;
var path_len: usize = 0;

/// Record where this process remembers its host: the state file's path plus
/// `suffix`. The composition root calls this once; null disables
/// remembering (no state directory could be resolved).
pub fn setPathFor(state_path: ?[]const u8) ?[]const u8 {
    const base = state_path orelse {
        path_len = 0;
        return null;
    };
    if (base.len == 0 or base.len + suffix.len > path_buffer.len) {
        path_len = 0;
        return null;
    }
    @memcpy(path_buffer[0..base.len], base);
    @memcpy(path_buffer[base.len..][0..suffix.len], suffix);
    path_len = base.len + suffix.len;
    return path_buffer[0..path_len];
}

pub fn path() ?[]const u8 {
    return if (path_len == 0) null else path_buffer[0..path_len];
}

pub fn encode(target: []const u8, out: []u8) ?[]const u8 {
    if (target.len == 0 or !config_module.validPhuxRemote(target)) return null;
    return std.fmt.bufPrint(out, header ++ key ++ "{s}\n", .{target}) catch null;
}

pub fn parse(bytes: []const u8) ?[]const u8 {
    if (!std.mem.startsWith(u8, bytes, header)) return null;
    const rest = bytes[header.len..];
    if (!std.mem.startsWith(u8, rest, key)) return null;
    const line_end = std.mem.indexOfScalar(u8, rest, '\n') orelse return null;
    if (line_end + 1 != rest.len) return null;
    const target = rest[key.len..line_end];
    if (target.len == 0 or !config_module.validPhuxRemote(target)) return null;
    return target;
}

/// The first remembered host, from either format, copied into `out`, or
/// null when there is none.
pub fn load(io: std.Io, file_path: []const u8, out: []u8) ?[]const u8 {
    var hosts: Hosts = .{};
    loadAll(io, file_path, &hosts);
    if (hosts.count == 0) return null;
    const target = hosts.get(0);
    if (target.len > out.len) return null;
    @memcpy(out[0..target.len], target);
    return out[0..target.len];
}

/// Remember `target`, or forget when it is null. Best effort by design: a
/// write that fails costs one Connect to Host after the next launch.
pub fn store(io: std.Io, file_path: []const u8, target: ?[]const u8) void {
    const cwd = std.Io.Dir.cwd();
    const value = target orelse {
        cwd.deleteFile(io, file_path) catch {};
        return;
    };
    var bytes: [max_file_bytes]u8 = undefined;
    const encoded = encode(value, &bytes) orelse return;
    // In a Phux-backed build nothing else writes the state directory, so the
    // first remembered host may be what creates it.
    if (std.fs.path.dirname(file_path)) |dir| cwd.createDirPath(io, dir) catch {};
    writeAtomic(io, file_path, encoded) catch {};
}
