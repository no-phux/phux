//! In-app Check for Updates. The TypeScript core owns chrome; this runs
//! scripts/cockpit-self-update.sh, which drives install-cockpit.sh.
const std = @import("std");

pub const request_name = "cockpit.update";
pub const max_bytes = 4096;
pub const Error = error{ InvalidRequest, BufferTooSmall, DriverMissing, BundleMissing, SpawnFailed, TimedOut };

pub const Action = enum(u8) { check = 0, install = 1 };
pub const Status = enum(u8) { current = 0, newer = 1, refused = 2, failed = 3, installed = 4 };

pub const Document = struct {
    status: Status = .failed,
    current: []const u8 = "",
    latest: []const u8 = "",
    message: []const u8 = "",
    remedy: []const u8 = "",
    applications_dir: []const u8 = "",
    relaunch: bool = false,

    fn canInstall(self: Document) bool {
        return self.status == .newer;
    }

    fn flags(self: Document) u8 {
        var value: u8 = 0;
        if (self.canInstall()) value |= 1;
        if (self.relaunch) value |= 2;
        return value;
    }
};

pub fn decode(bytes: []const u8) Error!Action {
    if (bytes.len != 2 or bytes[0] != 1) return error.InvalidRequest;
    return std.enums.fromInt(Action, bytes[1]) orelse error.InvalidRequest;
}

fn writeField(out: []u8, at: usize, field: []const u8) Error!usize {
    if (at + 2 + field.len > out.len) return error.BufferTooSmall;
    std.mem.writeInt(u16, out[at..][0..2], @intCast(field.len), .little);
    @memcpy(out[at + 2 ..][0..field.len], field);
    return at + 2 + field.len;
}

pub fn encode(doc: Document, out: []u8) Error![]const u8 {
    if (out.len < 3) return error.BufferTooSmall;
    out[0] = 1;
    out[1] = @intFromEnum(doc.status);
    out[2] = doc.flags();
    var at: usize = 3;
    at = try writeField(out, at, doc.current);
    at = try writeField(out, at, doc.latest);
    at = try writeField(out, at, doc.message);
    at = try writeField(out, at, doc.remedy);
    return out[0..at];
}

fn encodeFailed(out: []u8, message: []const u8) Error![]const u8 {
    return encode(.{ .status = .failed, .message = message }, out);
}

fn fieldValue(line: []const u8, key: []const u8) ?[]const u8 {
    if (line.len < key.len + 2) return null;
    if (!std.mem.startsWith(u8, line, key)) return null;
    if (!std.mem.startsWith(u8, line[key.len..], ": ")) return null;
    return line[key.len + 2 ..];
}

fn statusFrom(name: []const u8) Status {
    if (std.mem.eql(u8, name, "current")) return .current;
    if (std.mem.eql(u8, name, "newer")) return .newer;
    if (std.mem.eql(u8, name, "refused")) return .refused;
    if (std.mem.eql(u8, name, "installed")) return .installed;
    return .failed;
}

pub fn parseDocument(text: []const u8) Document {
    var doc: Document = .{};
    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |line| {
        if (fieldValue(line, "status")) |value| doc.status = statusFrom(value);
        if (fieldValue(line, "current")) |value| doc.current = value;
        if (fieldValue(line, "latest")) |value| doc.latest = value;
        if (fieldValue(line, "message")) |value| doc.message = value;
        if (fieldValue(line, "remedy")) |value| doc.remedy = value;
        if (fieldValue(line, "applications_dir")) |value| doc.applications_dir = value;
        if (fieldValue(line, "relaunch")) |value| doc.relaunch = std.mem.eql(u8, value, "yes");
    }
    if (doc.message.len == 0) doc.message = "updater produced no message";
    return doc;
}

pub fn enclosingApp(exe: []const u8) ?[]const u8 {
    const marker = ".app/Contents/MacOS/";
    const index = std.mem.lastIndexOf(u8, exe, marker) orelse return null;
    return exe[0 .. index + 4];
}

fn environment(name: [*:0]const u8) ?[]const u8 {
    return if (std.c.getenv(name)) |value| std.mem.span(value) else null;
}

fn fileExists(io: std.Io, path: []const u8) bool {
    std.Io.Dir.cwd().access(io, path, .{}) catch return false;
    return true;
}

pub fn driverPath(io: std.Io, buf: []u8, exe: []const u8) Error![]const u8 {
    if (environment("PHUX_COCKPIT_SELF_UPDATE")) |path| {
        if (fileExists(io, path)) return path;
    }
    if (std.fs.path.dirname(exe)) |macos_dir| {
        if (std.fs.path.dirname(macos_dir)) |contents| {
            const path = std.fmt.bufPrint(buf, "{s}/Resources/cockpit-self-update.sh", .{contents}) catch return error.DriverMissing;
            if (fileExists(io, path)) return path;
        }
    }
    const fallback = "scripts/cockpit-self-update.sh";
    if (fileExists(io, fallback)) return fallback;
    return error.DriverMissing;
}

pub fn bundlePath(io: std.Io, exe: []const u8) Error![]const u8 {
    if (environment("PHUX_COCKPIT_BUNDLE")) |path| {
        if (fileExists(io, path)) return path;
    }
    return enclosingApp(exe) orelse error.BundleMissing;
}

const Capture = struct {
    bytes: [max_bytes]u8 = undefined,
    len: usize = 0,
    fn text(self: *const Capture) []const u8 {
        return self.bytes[0..self.len];
    }
};

fn drain(fd: std.posix.fd_t, capture: *Capture) void {
    while (true) {
        var polls = [_]std.posix.pollfd{.{ .fd = fd, .events = std.posix.POLL.IN, .revents = 0 }};
        const ready = std.posix.poll(&polls, 0) catch return;
        if (ready == 0) return;
        var chunk: [512]u8 = undefined;
        const size = std.c.read(fd, &chunk, chunk.len);
        if (size <= 0) return;
        const take: usize = @intCast(size);
        const room = capture.bytes.len - capture.len;
        const copy = @min(take, room);
        @memcpy(capture.bytes[capture.len..][0..copy], chunk[0..copy]);
        capture.len += copy;
        if (copy < take) return;
    }
}

fn waitExit(id: std.posix.pid_t) ?u32 {
    var status: c_int = 0;
    const result = std.c.waitpid(id, &status, std.posix.W.NOHANG);
    if (result <= 0) return null;
    if (!std.posix.W.IFEXITED(@intCast(status))) return 1;
    return std.posix.W.EXITSTATUS(@intCast(status));
}

fn runDriver(io: std.Io, argv: []const []const u8, timeout_ms: u32) Error!Capture {
    var child = std.process.spawn(io, .{
        .argv = argv,
        .stdin = .ignore,
        .stdout = .pipe,
        .stderr = .pipe,
    }) catch return error.SpawnFailed;
    defer if (child.stdout) |file| file.close(io);
    defer if (child.stderr) |file| file.close(io);
    var stdout: Capture = .{};
    var stderr: Capture = .{};
    const started = std.Io.Clock.awake.now(io);
    while (true) {
        if (child.stdout) |file| drain(file.handle, &stdout);
        if (child.stderr) |file| drain(file.handle, &stderr);
        if (child.id) |id| {
            if (waitExit(id)) |_| {
                child.id = null;
                if (child.stdout) |file| drain(file.handle, &stdout);
                return stdout;
            }
        }
        if (started.durationTo(std.Io.Clock.awake.now(io)).toMilliseconds() >= timeout_ms) return error.TimedOut;
        std.Io.sleep(io, .fromMilliseconds(10), .awake) catch return error.SpawnFailed;
    }
}

extern "c" fn _NSGetExecutablePath(buf: [*c]u8, bufsize: *u32) c_int;

pub fn executablePath(buf: []u8) Error![]u8 {
    var size: u32 = @intCast(buf.len);
    if (_NSGetExecutablePath(buf.ptr, &size) != 0) return error.BundleMissing;
    const len = std.mem.indexOfScalar(u8, buf, 0) orelse buf.len;
    return buf[0..len];
}

pub fn handle(io: std.Io, payload: []const u8, out: []u8) Error![]const u8 {
    const action = decode(payload) catch return encodeFailed(out, "invalid update request");
    var exe_buf: [std.fs.max_path_bytes]u8 = undefined;
    const exe = executablePath(&exe_buf) catch return encodeFailed(out, "could not locate this Cockpit executable");
    var driver_buf: [std.fs.max_path_bytes]u8 = undefined;
    const driver = driverPath(io, &driver_buf, exe) catch return encodeFailed(out, "cockpit-self-update.sh is not installed beside this app");
    const bundle = bundlePath(io, exe) catch return encodeFailed(out, "this process is not running inside Phux Cockpit.app");
    const verb: []const u8 = if (action == .install) "--install" else "--check";
    const argv = [_][]const u8{ driver, verb, "--bundle", bundle };
    const timeout: u32 = if (action == .install) 120_000 else 30_000;
    const stdout = runDriver(io, &argv, timeout) catch |err| {
        const message: []const u8 = switch (err) {
            error.TimedOut => "the updater timed out",
            else => "could not start cockpit-self-update.sh",
        };
        return encodeFailed(out, message);
    };
    return encode(parseDocument(stdout.text()), out);
}

test "enclosing app is the bundle, not the executable" {
    const exe = "/Applications/Phux Cockpit.app/Contents/MacOS/phux-cockpit";
    try std.testing.expectEqualStrings("/Applications/Phux Cockpit.app", enclosingApp(exe).?);
    try std.testing.expect(enclosingApp("/usr/local/bin/phux-cockpit") == null);
}

test "document parse and wire round-trip" {
    const text =
        \\status: newer
        \\current: 0.24.0
        \\latest: cockpit-v0.25.0
        \\message: Phux Cockpit 0.25.0 is available (you have 0.24.0).
        \\remedy:
        \\relaunch: no
        \\
    ;
    const doc = parseDocument(text);
    try std.testing.expectEqual(Status.newer, doc.status);
    try std.testing.expectEqualStrings("0.24.0", doc.current);
    try std.testing.expectEqualStrings("cockpit-v0.25.0", doc.latest);
    var buf: [max_bytes]u8 = undefined;
    const encoded = try encode(doc, &buf);
    try std.testing.expectEqual(@as(u8, 1), encoded[0]);
    try std.testing.expectEqual(@as(u8, 1), encoded[1]);
    try std.testing.expectEqual(@as(u8, 1), encoded[2]);
    try std.testing.expectEqual(Action.check, try decode(&.{ 1, 0 }));
    try std.testing.expectEqual(Action.install, try decode(&.{ 1, 1 }));
    try std.testing.expectError(error.InvalidRequest, decode(&.{1}));
}

test "homebrew refusal stays a document, not a transport failure" {
    const doc = parseDocument("status: refused\nmessage: Homebrew.\nremedy: brew upgrade --cask no-phux/tap/phux-cockpit\n");
    try std.testing.expectEqual(Status.refused, doc.status);
    try std.testing.expect(!doc.canInstall());
    var buf: [max_bytes]u8 = undefined;
    const encoded = try encode(doc, &buf);
    try std.testing.expectEqual(@as(u8, 2), encoded[1]);
    try std.testing.expectEqual(@as(u8, 0), encoded[2]);
}
