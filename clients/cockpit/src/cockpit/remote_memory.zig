//! The one line Cockpit remembers about a remote host across launches.
//!
//! Phux-backed layout lives in the coordinator's shared workspace, not in
//! Cockpit's state file, so the only client-side fact a relaunch needs is
//! WHICH coordinator to reattach to. When Connect to Host attaches a
//! registered host, its label is written beside the workspace state file;
//! returning to this Mac removes it. At startup a remembered label selects
//! that host unless the config or environment names one explicitly.
//!
//! The format is a fixed header plus exactly one `target=` line. Anything
//! else (a torn write, a hand edit, an older or newer format) is treated as
//! absent: the cost of forgetting is one Connect to Host, and the cost of
//! misreading is dialing a host nobody chose.

const std = @import("std");
const config_module = @import("../config/config.zig");

pub const suffix = ".remote";
pub const header = "phux-cockpit-remote v1\n";
const key = "target=";
pub const max_file_bytes = header.len + key.len + config_module.max_phux_remote_bytes + 1;

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

/// The remembered label copied into `out`, or null when there is none.
pub fn load(io: std.Io, file_path: []const u8, out: []u8) ?[]const u8 {
    var file = std.Io.Dir.cwd().openFile(io, file_path, .{}) catch return null;
    defer file.close(io);
    var bytes: [max_file_bytes + 1]u8 = undefined;
    const read = file.readPositionalAll(io, &bytes, 0) catch return null;
    const target = parse(bytes[0..read]) orelse return null;
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
