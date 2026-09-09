//! Local coordinator bootstrap. Call only from the socket worker, before connect.
//! The CLI owns coordinator policy; this helper never attaches or spawns work.
const std = @import("std");

pub const Options = struct {
    /// Explicit absolute fixture/tool path. Null resolves only the bundled peer.
    cli_path: ?[]const u8 = null,
    /// Outer fail-safe exceeds the CLI's ten-second ensure deadline. Cancellation
    /// is checked every 10ms so stopping a worker does not wait for this budget.
    timeout_ms: u32 = 15_000,
};

pub fn siblingPath(gpa: std.mem.Allocator, executable: []const u8) ![]u8 {
    if (!std.fs.path.isAbsolute(executable)) return error.InvalidExecutablePath;
    const directory = std.fs.path.dirname(executable) orelse return error.InvalidExecutablePath;
    return std.fs.path.join(gpa, &.{ directory, "phux" });
}

fn discover(gpa: std.mem.Allocator, io: std.Io, explicit: ?[]const u8) ![]u8 {
    if (explicit) |path| {
        if (!std.fs.path.isAbsolute(path)) return error.InvalidExecutablePath;
        return gpa.dupe(u8, path);
    }
    const executable = try std.process.executablePathAlloc(io, gpa);
    defer gpa.free(executable);
    return siblingPath(gpa, executable);
}

pub fn ensure(
    gpa: std.mem.Allocator,
    io: std.Io,
    socket_path: []const u8,
    stopping: *const std.atomic.Value(bool),
    options: Options,
) !void {
    if (!std.fs.path.isAbsolute(socket_path)) return error.InvalidSocketPath;
    if (std.mem.indexOfScalar(u8, socket_path, 0) != null) return error.InvalidSocketPath;
    if (stopping.load(.acquire)) return error.Canceled;
    const cli = try discover(gpa, io, options.cli_path);
    defer gpa.free(cli);
    // No shell, PATH search, inherited terminal, or output pipes that could fill.
    // In particular, never retry after an uncertain helper exit.
    var child = try std.process.spawn(io, .{
        .argv = &.{ cli, "--socket", socket_path, "server", "--ensure" },
        .stdin = .ignore,
        .stdout = .ignore,
        .stderr = .ignore,
        // Temporary launchctl/systemctl children share this disposable group.
        // A daemonized Phux calls setsid and owns its independent lifetime.
        .pgid = 0,
    });
    defer terminate(io, &child);
    try wait(io, &child, stopping, options.timeout_ms);
}

fn terminate(io: std.Io, child: *std.process.Child) void {
    const pid = child.id orelse return;
    // Zig 0.16 Child.kill sends SIGTERM and waits indefinitely. A wedged helper
    // may ignore TERM; kill the disposable group so supervisor probes cannot
    // outlive cancellation either. The coordinator detaches with setsid.
    _ = std.c.kill(-pid, .KILL);
    _ = child.wait(io) catch {};
}

fn wait(io: std.Io, child: *std.process.Child, stopping: *const std.atomic.Value(bool), timeout_ms: u32) !void {
    const started = std.Io.Clock.awake.now(io);
    while (true) {
        if (stopping.load(.acquire)) return error.Canceled;
        if (started.durationTo(std.Io.Clock.awake.now(io)).toMilliseconds() >= timeout_ms)
            return error.EnsureTimedOut;
        if (try exited(child)) return;
        try std.Io.sleep(io, .fromMilliseconds(10), .awake);
    }
}

/// Reap exactly our child. Clear its ID before interpreting failure so deferred
/// cleanup cannot signal a recycled PID. All stdio is ignored (no pipe handles).
fn exited(child: *std.process.Child) !bool {
    var status: c_int = 0;
    const result = std.c.waitpid(child.id.?, &status, std.posix.W.NOHANG);
    if (result == 0) return false;
    if (result < 0) {
        if (std.posix.errno(result) == .INTR) return false;
        // Another reaper may have consumed this PID. It is no longer ours to
        // signal or wait on (the threaded wait implementation panics on ECHILD).
        if (std.posix.errno(result) == .CHILD) child.id = null;
        return error.EnsureWaitFailed;
    }
    child.id = null;
    if (!std.posix.W.IFEXITED(@intCast(status))) return error.EnsureFailed;
    if (std.posix.W.EXITSTATUS(@intCast(status)) != 0) return error.EnsureFailed;
    return true;
}

test "CLI discovery stays beside the executable including bundle paths with spaces" {
    const gpa = std.testing.allocator;
    const path = try siblingPath(gpa, "/Applications/Phux Cockpit.app/Contents/MacOS/phux-cockpit");
    defer gpa.free(path);
    try std.testing.expectEqualStrings("/Applications/Phux Cockpit.app/Contents/MacOS/phux", path);
    try std.testing.expectError(error.InvalidExecutablePath, siblingPath(gpa, "phux-cockpit"));
}

test "ensure rejects relative sockets and canceled startup before executing a fixture" {
    var stopping = std.atomic.Value(bool).init(false);
    const options: Options = .{ .cli_path = "/does-not-exist/phux" };
    try std.testing.expectError(error.InvalidSocketPath, ensure(std.testing.allocator, std.testing.io, "relative.sock", &stopping, options));
    stopping.store(true, .release);
    try std.testing.expectError(error.Canceled, ensure(std.testing.allocator, std.testing.io, "/unused.sock", &stopping, options));
}

test "ensure observes fixture failure without fallback or retry" {
    var stopping = std.atomic.Value(bool).init(false);
    try std.testing.expectError(error.EnsureFailed, ensure(std.testing.allocator, std.testing.io, "/unused.sock", &stopping, .{ .cli_path = "/usr/bin/false" }));
    try ensure(std.testing.allocator, std.testing.io, "/unused.sock", &stopping, .{ .cli_path = "/usr/bin/true" });
}

/// Test-only process fixture shared with the worker integration test. Every path
/// lives in a disposable directory; the selected socket is never created.
pub const TestFixture = struct {
    tmp: std.testing.TmpDir,
    cli: [:0]u8,
    socket: []u8,

    pub fn init() !TestFixture {
        var tmp = std.testing.tmpDir(.{});
        errdefer tmp.cleanup();
        try tmp.dir.writeFile(std.testing.io, .{
            .sub_path = "fixture cli",
            .data = @embedFile("fixtures/ensure-cli.py"),
            .flags = .{ .permissions = .fromMode(0o700) },
        });
        const cli = try tmp.dir.realPathFileAlloc(std.testing.io, "fixture cli", std.testing.allocator);
        errdefer std.testing.allocator.free(cli);
        const socket = try std.fs.path.join(std.testing.allocator, &.{ std.fs.path.dirname(cli).?, "selected socket" });
        return .{ .tmp = tmp, .cli = cli, .socket = socket };
    }

    pub fn deinit(self: *TestFixture) void {
        std.testing.allocator.free(self.cli);
        std.testing.allocator.free(self.socket);
        self.tmp.cleanup();
    }

    pub fn ready(self: *TestFixture) bool {
        self.tmp.dir.access(std.testing.io, "pid", .{}) catch return false;
        return true;
    }

    pub fn release(self: *TestFixture, code: []const u8) !void {
        try self.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "release.tmp", .data = code });
        try self.tmp.dir.rename("release.tmp", self.tmp.dir, "release", std.testing.io);
    }

    pub fn expectExitCode(self: *TestFixture, expected: []const u8) !void {
        const code = try self.tmp.dir.readFileAlloc(std.testing.io, "exit-code", std.testing.allocator, .limited(32));
        defer std.testing.allocator.free(code);
        try std.testing.expectEqualStrings(expected, code);
    }

    pub fn checkArguments(self: *TestFixture) !void {
        const actual = try self.tmp.dir.readFileAlloc(std.testing.io, "calls", std.testing.allocator, .limited(4096));
        defer std.testing.allocator.free(actual);
        const expected = try std.fmt.allocPrint(std.testing.allocator, "--socket\n{s}\nserver\n--ensure\n", .{self.socket});
        defer std.testing.allocator.free(expected);
        try std.testing.expectEqualStrings(expected, actual);
    }

    pub fn expectReaped(self: *TestFixture) !void {
        const text = try self.tmp.dir.readFileAlloc(std.testing.io, "pid", std.testing.allocator, .limited(32));
        defer std.testing.allocator.free(text);
        const pid = try std.fmt.parseInt(std.posix.pid_t, text, 10);
        var status: c_int = 0;
        try std.testing.expectEqual(@as(std.posix.pid_t, -1), std.c.waitpid(pid, &status, std.posix.W.NOHANG));
        try std.testing.expectEqual(std.posix.E.CHILD, std.posix.errno(-1));
    }

    fn expectProbeStopped(self: *TestFixture) !void {
        const text = try self.tmp.dir.readFileAlloc(std.testing.io, "probe-pid", std.testing.allocator, .limited(32));
        defer std.testing.allocator.free(text);
        const pid = try std.fmt.parseInt(std.posix.pid_t, text, 10);
        const started = std.Io.Clock.awake.now(std.testing.io);
        while (std.posix.errno(std.c.kill(pid, @enumFromInt(0))) != .SRCH) {
            if (started.durationTo(std.Io.Clock.awake.now(std.testing.io)).toMilliseconds() >= 5000) {
                // A failing test must not leave its owned disposable probe alive.
                _ = std.c.kill(pid, .KILL);
                return error.ProbeSurvivedCancellation;
            }
            try std.Io.sleep(std.testing.io, .fromMilliseconds(10), .awake);
        }
    }
};

test "ensure bounds an unresponsive helper and reaps it" {
    var fixture = try TestFixture.init();
    defer fixture.deinit();
    try fixture.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "spawn-probe", .data = "" });
    var stopping = std.atomic.Value(bool).init(false);
    // The fixture runs until released; allow Python startup, then enforce the
    // outer budget. This is a timeout contract, not a performance assertion.
    try std.testing.expectError(error.EnsureTimedOut, ensure(std.testing.allocator, std.testing.io, fixture.socket, &stopping, .{ .cli_path = fixture.cli, .timeout_ms = 1000 }));
    try std.testing.expect(fixture.ready());
    try fixture.checkArguments();
    try fixture.expectReaped();
    try fixture.expectProbeStopped();
}

test "failed fixture ensure runs once and reports failure" {
    var fixture = try TestFixture.init();
    defer fixture.deinit();
    try fixture.release("7");
    var stopping = std.atomic.Value(bool).init(false);
    try std.testing.expectError(error.EnsureFailed, ensure(std.testing.allocator, std.testing.io, fixture.socket, &stopping, .{ .cli_path = fixture.cli }));
    // The fixture appends one full argv per invocation; any retry changes this.
    try fixture.checkArguments();
    try fixture.expectReaped();
}

test "already reaped helper is never signaled again" {
    const io = std.testing.io;
    var child = try std.process.spawn(io, .{ .argv = &.{"/usr/bin/true"}, .stdin = .ignore, .stdout = .ignore, .stderr = .ignore });
    var stale = child;
    _ = try child.wait(io);
    try std.testing.expectError(error.EnsureWaitFailed, exited(&stale));
    try std.testing.expectEqual(null, stale.id);
    terminate(io, &stale);
}
