//! The remote hosts Cockpit remembers across launches.
//!
//! Phux-backed layout lives in each coordinator's shared workspace, not in
//! Cockpit's state file, so the only client-side facts a relaunch needs are
//! WHICH coordinators to reattach to. Each registered host Connect to Host
//! attaches joins a short list beside the workspace state file; Disconnect
//! removes that host and Disconnect All removes the file. At startup every
//! remembered host is reattached beside this Mac, listing: none is attached
//! until one of its sessions is shown.
//!
//! Two formats. v1, what earlier releases wrote, is a fixed header plus
//! exactly one `target=` line. v2 is a fixed header plus one to `max_hosts`
//! distinct `target=` lines, oldest first. Anything else (a torn write, a
//! hand edit, an unknown format) is treated as absent: the cost of
//! forgetting is one Connect to Host, and the cost of misreading is dialing
//! a host nobody chose.

const std = @import("std");
const config_module = @import("../config/config.zig");

pub const suffix = ".remote";
pub const header = "phux-cockpit-remote v1\n";
pub const list_header = "phux-cockpit-remote v2\n";
const key = "target=";
/// This Mac plus three hosts is Cockpit's coordinator bound.
pub const max_hosts: usize = 3;
pub const max_file_bytes = header.len + key.len + config_module.max_phux_remote_bytes + 1;
pub const max_list_file_bytes = list_header.len + max_hosts * (key.len + config_module.max_phux_remote_bytes + 1);

/// Remembered hosts, oldest first, each a valid `phux-remote` value.
pub const Hosts = struct {
    storage: [max_hosts][config_module.max_phux_remote_bytes]u8 = undefined,
    lens: [max_hosts]usize = @splat(0),
    count: usize = 0,

    pub fn get(self: *const Hosts, index: usize) []const u8 {
        return self.storage[index][0..self.lens[index]];
    }

    pub fn contains(self: *const Hosts, target: []const u8) bool {
        for (0..self.count) |index| {
            if (std.mem.eql(u8, self.get(index), target)) return true;
        }
        return false;
    }

    /// False when `target` is invalid, already listed, or the list is full.
    pub fn add(self: *Hosts, target: []const u8) bool {
        if (target.len == 0 or target.len > config_module.max_phux_remote_bytes) return false;
        if (!config_module.validPhuxRemote(target)) return false;
        if (self.contains(target) or self.count == max_hosts) return false;
        @memcpy(self.storage[self.count][0..target.len], target);
        self.lens[self.count] = target.len;
        self.count += 1;
        return true;
    }

    /// False when `target` was not listed. The others keep their order.
    pub fn remove(self: *Hosts, target: []const u8) bool {
        for (0..self.count) |index| {
            if (!std.mem.eql(u8, self.get(index), target)) continue;
            for (index + 1..self.count) |next| {
                self.storage[next - 1] = self.storage[next];
                self.lens[next - 1] = self.lens[next];
            }
            self.count -= 1;
            return true;
        }
        return false;
    }
};

/// The remembered hosts, from either format, into `out`: false, with `out`
/// empty, for anything malformed.
pub fn parseAll(bytes: []const u8, out: *Hosts) bool {
    out.* = .{};
    if (parse(bytes)) |target| return out.add(target);
    if (!std.mem.startsWith(u8, bytes, list_header)) return false;
    var rest = bytes[list_header.len..];
    while (rest.len != 0) {
        const line_end = std.mem.indexOfScalar(u8, rest, '\n') orelse return reject(out);
        const line = rest[0..line_end];
        if (!std.mem.startsWith(u8, line, key) or !out.add(line[key.len..])) return reject(out);
        rest = rest[line_end + 1 ..];
    }
    return out.count != 0 or reject(out);
}

fn reject(out: *Hosts) bool {
    out.* = .{};
    return false;
}

/// The v2 file for `hosts`, or null when there is nothing to remember.
pub fn encodeAll(hosts: *const Hosts, out: []u8) ?[]const u8 {
    if (hosts.count == 0) return null;
    var at = append(out, 0, list_header) orelse return null;
    for (0..hosts.count) |index| {
        at = append(out, at, key) orelse return null;
        at = append(out, at, hosts.get(index)) orelse return null;
        at = append(out, at, "\n") orelse return null;
    }
    return out[0..at];
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
    const cwd = std.Io.Dir.cwd();
    var bytes: [max_list_file_bytes]u8 = undefined;
    const encoded = encodeAll(hosts, &bytes) orelse {
        cwd.deleteFile(io, file_path) catch {};
        return;
    };
    if (std.fs.path.dirname(file_path)) |dir| cwd.createDirPath(io, dir) catch {};
    cwd.writeFile(io, .{ .sub_path = file_path, .data = encoded }) catch {};
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
    cwd.writeFile(io, .{ .sub_path = file_path, .data = encoded }) catch {};
}
