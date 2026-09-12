//! Local coordinator bootstrap and optional background CLI discovery.
//! Ensure runs before connect; optional discovery runs independently after attach.
//! The CLI owns coordinator policy; this helper never attaches or spawns work.
const std = @import("std");
const wait_api = @cImport({
    @cInclude("sys/wait.h");
});

pub const Options = struct {
    /// Trusted same-checkout fixture override. Production discovery uses PHUX_CLI.
    cli_path: ?[]const u8 = null,
    /// Outer fail-safe exceeds the CLI's ten-second ensure deadline. Cancellation
    /// is checked every 10ms so stopping a worker does not wait for this budget.
    timeout_ms: u32 = 15_000,
    evidence: ?*Evidence = null,
    status: ?*Status = null,
};

/// Provider-owned, survives worker teardown. Worker publishes bounded data under
/// a short lock; UI copies it, never borrows mutable worker memory.
pub const Status = struct {
    mutex: std.atomic.Mutex = .unlocked,
    evidence: Evidence = .{},
    message: [1024]u8 = undefined,
    message_len: usize = 0,

    fn lock(self: *const Status) *Status {
        const mutable: *Status = @constCast(self);
        while (!mutable.mutex.tryLock()) std.atomic.spinLoopHint();
        return mutable;
    }

    pub fn record(self: *Status, evidence: Evidence, socket: []const u8, failure: ?anyerror) void {
        const locked = self.lock();
        defer locked.mutex.unlock();
        locked.evidence = evidence;
        locked.message_len = 0;
        if (failure) |err| {
            const text = std.fmt.bufPrint(&locked.message, "Local Phux at {s}: {s}; CLI {s}. {s} Retry or Repair Installation.", .{ socket, @errorName(err), evidence.cli(), evidence.stderr.text()[0..@min(240, evidence.stderr.len)] }) catch {
                const fallback = "Local Phux startup failed. Retry or Repair Installation.";
                @memcpy(locked.message[0..fallback.len], fallback);
                locked.message_len = fallback.len;
                return;
            };
            locked.message_len = text.len;
        }
    }

    pub fn cliInto(self: *const Status, out: []u8) ?[]const u8 {
        const locked = self.lock();
        defer locked.mutex.unlock();
        const cli = locked.evidence.cli();
        if (cli.len == 0 or cli.len > out.len) return null;
        @memcpy(out[0..cli.len], cli);
        return out[0..cli.len];
    }

    pub fn failureInto(self: *const Status, out: []u8) []const u8 {
        const locked = self.lock();
        defer locked.mutex.unlock();
        const len = @min(out.len, locked.message_len);
        @memcpy(out[0..len], locked.message[0..len]);
        return out[0..len];
    }
};

/// Bounded launch evidence, owned by the caller rather than a global last error.
pub const Evidence = struct {
    executable: [4096]u8 = undefined,
    executable_len: usize = 0,
    stderr: Capture = .{},
    status: ?c_int = null,
    pub fn cli(self: *const Evidence) []const u8 {
        return self.executable[0..self.executable_len];
    }
};

pub const Capture = struct {
    bytes: [4096]u8 = undefined,
    len: usize = 0,
    pub fn text(self: *const Capture) []const u8 {
        return self.bytes[0..self.len];
    }
};

/// Optional CLI discovery is independent of the coordinator socket pump. Its
/// owner cancels and joins it before destroying provider-owned status/slices.
pub const Discovery = struct {
    gpa: std.mem.Allocator,
    io: std.Io,
    status: *Status,
    socket: []const u8,
    stopping: std.atomic.Value(bool) = .init(false),
    thread: ?std.Thread = null,

    pub fn start(gpa: std.mem.Allocator, io: std.Io, socket: []const u8, status: ?*Status) !?*Discovery {
        const target = status orelse return null;
        var path: [4096]u8 = undefined;
        if (target.cliInto(&path) != null) return null;
        const self = try gpa.create(Discovery);
        errdefer gpa.destroy(self);
        self.* = .{ .gpa = gpa, .io = io, .status = target, .socket = socket };
        self.thread = try std.Thread.spawn(.{}, run, .{self});
        return self;
    }

    pub fn stop(self: *Discovery) void {
        self.stopping.store(true, .release);
        if (self.thread) |thread| thread.join();
        self.gpa.destroy(self);
    }

    fn run(self: *Discovery) void {
        const cli = discoverRuntime(self.gpa, self.io, &self.stopping) catch return;
        defer self.gpa.free(cli);
        if (self.stopping.load(.acquire)) return;
        var evidence: Evidence = .{};
        evidence.executable_len = @min(cli.len, evidence.executable.len);
        @memcpy(evidence.executable[0..evidence.executable_len], cli[0..evidence.executable_len]);
        self.status.record(evidence, self.socket, null);
    }
};

pub fn environment(name: [*:0]const u8) ?[]const u8 {
    const value = std.c.getenv(name) orelse return null;
    const text = std.mem.span(value);
    return if (text.len == 0) null else text;
}

pub fn siblingPath(gpa: std.mem.Allocator, executable: []const u8) ![]u8 {
    if (!std.fs.path.isAbsolute(executable)) return error.InvalidExecutablePath;
    const directory = std.fs.path.dirname(executable) orelse return error.InvalidExecutablePath;
    return std.fs.path.join(gpa, &.{ directory, "phux" });
}

fn discover(gpa: std.mem.Allocator, io: std.Io, explicit: ?[]const u8, stopping: *const std.atomic.Value(bool)) ![]u8 {
    if (explicit) |path| {
        if (!std.fs.path.isAbsolute(path)) return error.InvalidExecutablePath;
        return gpa.dupe(u8, path);
    }
    return discoverRuntime(gpa, io, stopping);
}

/// Finder-safe deterministic candidates. Never source shell startup files. The
/// total budget is 1s per candidate, at most 8 PATH entries plus 4 fixed paths.
pub fn discoverRuntime(gpa: std.mem.Allocator, io: std.Io, stopping: *const std.atomic.Value(bool)) ![]u8 {
    var arena = std.heap.ArenaAllocator.init(gpa);
    defer arena.deinit();
    var candidates: Candidates = .{ .gpa = arena.allocator() };
    try candidates.fromEnvironment();
    if (try selectCompatible(gpa, io, candidates.paths.items, stopping)) |cli| return cli;
    const executable = try std.process.executablePathAlloc(io, gpa);
    defer gpa.free(executable);
    return siblingPath(gpa, executable);
}

fn selectCompatible(gpa: std.mem.Allocator, io: std.Io, candidates: []const []const u8, stopping: *const std.atomic.Value(bool)) !?[]u8 {
    for (candidates) |path| {
        if (try compatibleCandidate(gpa, io, path, stopping)) return try gpa.dupe(u8, path);
    }
    return null;
}

/// Operation-arena owned. Duplicate paths consume neither a second process nor
/// another timeout budget. Keep path spelling: normalizing '..' across a symlink
/// could name a different executable.
const Candidates = struct {
    gpa: std.mem.Allocator,
    paths: std.ArrayList([]const u8) = .empty,

    fn add(self: *Candidates, path: []const u8) !void {
        if (path.len > 4096 or !std.fs.path.isAbsolute(path)) return;
        for (self.paths.items) |existing| {
            if (std.mem.eql(u8, existing, path)) return;
        }
        try self.paths.append(self.gpa, try self.gpa.dupe(u8, path));
    }

    fn addPath(self: *Candidates, path: []const u8) !void {
        var entries = std.mem.splitScalar(u8, path, ':');
        var count: usize = 0;
        while (entries.next()) |directory| {
            if (count == 8) break;
            if (!std.fs.path.isAbsolute(directory)) continue;
            count += 1;
            if (directory.len > 4090) continue;
            try self.add(try std.fs.path.join(self.gpa, &.{ directory, "phux" }));
        }
    }

    fn fromEnvironment(self: *Candidates) !void {
        if (environment("PHUX_CLI")) |path| try self.add(path);
        if (environment("PATH")) |path| try self.addPath(path);
        if (environment("HOME")) |home| {
            if (home.len <= 4096) try self.add(try std.fs.path.join(self.gpa, &.{ home, ".local/bin/phux" }));
        }
        try self.add("/opt/homebrew/bin/phux");
        try self.add("/usr/local/bin/phux");
    }
};

fn compatibleCandidate(gpa: std.mem.Allocator, io: std.Io, path: []const u8, stopping: *const std.atomic.Value(bool)) !bool {
    if (stopping.load(.acquire)) return error.Canceled;
    if (!std.fs.path.isAbsolute(path)) return false;
    var stdout: Capture = .{};
    var evidence: Evidence = .{};
    runHelper(io, &.{ path, "runtime-info", "--json" }, stopping, 1000, &stdout, &evidence) catch |err| {
        if (err == error.Canceled) return err;
        return false;
    };
    return compatibleProbe(gpa, stdout.text());
}

/// Exact wire minor and explicit behavior capabilities, never package semver.
pub fn compatibleProbe(gpa: std.mem.Allocator, json: []const u8) bool {
    const Probe = struct {
        schema_version: u32,
        binary: []const u8,
        protocol: struct { major: u32, minor: u32, patch: u32 },
        capabilities: []const []const u8,
    };
    const parsed = std.json.parseFromSlice(Probe, gpa, json, .{ .ignore_unknown_fields = true }) catch return false;
    defer parsed.deinit();
    const p = parsed.value;
    if (p.schema_version != 1 or !std.mem.eql(u8, p.binary, "phux")) return false;
    // Same contract as crates/phux-protocol/src/lib.rs; runtime handshake is
    // still authoritative for the running coordinator's negotiated features.
    if (p.protocol.major != 0 or p.protocol.minor != 9) return false;
    for ([_][]const u8{ "server-ensure-v1", "structured-spawn-v1", "host-enroll-v1" }) |required| {
        if (!hasCapability(p.capabilities, required)) return false;
    }
    return true;
}

fn hasCapability(capabilities: []const []const u8, required: []const u8) bool {
    for (capabilities) |capability| {
        if (std.mem.eql(u8, capability, required)) return true;
    }
    return false;
}

/// A reachable existing socket wins, including a server needing a newer app.
/// Negotiation belongs to the provider; bootstrap cannot replace that server.
pub fn serverPresent(path: []const u8) !bool {
    if (path.len >= @sizeOf(@FieldType(std.posix.sockaddr.un, "path"))) return error.InvalidSocketPath;
    var address = std.mem.zeroes(std.posix.sockaddr.un);
    address.len = @intCast(@offsetOf(std.posix.sockaddr.un, "path") + path.len + 1);
    address.family = std.posix.AF.UNIX;
    @memcpy(address.path[0..path.len], path);
    const fd = std.c.socket(std.posix.AF.UNIX, std.posix.SOCK.STREAM, 0);
    if (fd < 0) return error.SocketOpenFailed;
    defer _ = std.c.close(fd);
    if (std.c.fcntl(fd, std.posix.F.SETFL, @as(c_int, @bitCast(std.posix.O{ .NONBLOCK = true }))) < 0) return error.SocketProbeFailed;
    const rc = std.c.connect(fd, @ptrCast(&address), address.len);
    if (rc == 0) return true;
    return switch (std.posix.errno(rc)) {
        .NOENT, .CONNREFUSED => false,
        .INPROGRESS, .AGAIN => true,
        else => error.SocketProbeFailed,
    };
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
    if (try serverPresent(socket_path)) return;
    const cli = try discover(gpa, io, options.cli_path, stopping);
    defer gpa.free(cli);
    var fallback: Evidence = .{};
    const evidence = options.evidence orelse &fallback;
    evidence.* = .{};
    evidence.executable_len = @min(cli.len, evidence.executable.len);
    @memcpy(evidence.executable[0..evidence.executable_len], cli[0..evidence.executable_len]);
    var stdout: Capture = .{};
    try runHelper(io, &.{ cli, "--socket", socket_path, "server", "--ensure" }, stopping, options.timeout_ms, &stdout, evidence);
}

fn runHelper(io: std.Io, argv: []const []const u8, stopping: *const std.atomic.Value(bool), timeout_ms: u32, stdout: *Capture, evidence: *Evidence) !void {
    var child = try std.process.spawn(io, .{
        .argv = argv,
        .stdin = .ignore,
        .stdout = .pipe,
        .stderr = .pipe,
        // Temporary launchctl/systemctl children share this disposable group.
        // A daemonized Phux calls setsid and owns its independent lifetime.
        .pgid = 0,
    });
    defer terminate(io, &child);
    defer closePipes(io, &child);
    const started = std.Io.Clock.awake.now(io);
    while (true) {
        try drain(child.stdout.?.handle, stdout);
        try drain(child.stderr.?.handle, &evidence.stderr);
        if (stopping.load(.acquire)) return error.Canceled;
        if (started.durationTo(std.Io.Clock.awake.now(io)).toMilliseconds() >= timeout_ms) return error.EnsureTimedOut;
        const done = exitedWithEvidence(&child, &evidence.status) catch |err| {
            try drain(child.stderr.?.handle, &evidence.stderr);
            return err;
        };
        if (done) {
            try drain(child.stdout.?.handle, stdout);
            try drain(child.stderr.?.handle, &evidence.stderr);
            return;
        }
        try std.Io.sleep(io, .fromMilliseconds(10), .awake);
    }
}

fn closePipes(io: std.Io, child: *std.process.Child) void {
    if (child.stdout) |file| file.close(io);
    if (child.stderr) |file| file.close(io);
    child.stdout = null;
    child.stderr = null;
}

fn drain(fd: std.posix.fd_t, capture: *Capture) !void {
    while (true) {
        var polls = [_]std.posix.pollfd{.{ .fd = fd, .events = std.posix.POLL.IN, .revents = 0 }};
        if (try std.posix.poll(&polls, 0) == 0) return;
        var chunk: [1024]u8 = undefined;
        const size = std.c.read(fd, &chunk, chunk.len);
        if (size == 0) return;
        if (size < 0) return error.HelperReadFailed;
        const n: usize = @intCast(size);
        const retained = @min(n, capture.bytes.len - capture.len);
        @memcpy(capture.bytes[capture.len..][0..retained], chunk[0..retained]);
        capture.len += retained;
        if (retained < n) return error.HelperOutputTooLarge;
    }
}

fn terminate(io: std.Io, child: *std.process.Child) void {
    const pid = child.id orelse return;
    // Zig 0.16 Child.kill sends SIGTERM and waits indefinitely. A wedged helper
    // may ignore TERM; kill the disposable group so supervisor probes cannot
    // outlive cancellation either. The coordinator detaches with setsid.
    _ = std.c.kill(-pid, .KILL);
    _ = child.wait(io) catch {};
}

/// The helper's unreaped leader pins its PID/PGID until group cleanup is done.
/// WNOWAIT observes exit without giving that identity back to the OS. A Phux
/// daemon that called setsid is outside this disposable process group.
fn exited(child: *std.process.Child) !bool {
    var status: ?c_int = null;
    return exitedWithEvidence(child, &status);
}

fn exitedWithEvidence(child: *std.process.Child, recorded: *?c_int) !bool {
    if (!try leaderExited(child)) return false;
    _ = std.c.kill(-child.id.?, .KILL);
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
    recorded.* = status;
    if (!std.posix.W.IFEXITED(@intCast(status))) return error.EnsureFailed;
    if (std.posix.W.EXITSTATUS(@intCast(status)) != 0) return error.EnsureFailed;
    return true;
}

fn leaderExited(child: *std.process.Child) !bool {
    var info = std.mem.zeroes(wait_api.siginfo_t);
    const rc = wait_api.waitid(wait_api.P_PID, @intCast(child.id.?), &info, wait_api.WEXITED | wait_api.WNOHANG | wait_api.WNOWAIT);
    if (rc == 0) return info.si_pid != 0;
    if (std.posix.errno(rc) == .INTR) return false;
    if (std.posix.errno(rc) == .CHILD) child.id = null;
    return error.EnsureWaitFailed;
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
        const socket = try std.fs.path.join(std.testing.allocator, &.{ std.fs.path.dirname(cli).?, "s s" });
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

const test_probe =
    \\{"schema_version":1,"binary":"phux","version":"unknown-development-version","protocol":{"major":0,"minor":9,"patch":0},"capabilities":["server-ensure-v1","structured-spawn-v1","host-enroll-v1"]}
;

test "runtime compatibility uses versioned protocol evidence rather than CLI semver" {
    const gpa = std.testing.allocator;
    try std.testing.expect(compatibleProbe(gpa, test_probe));
    try std.testing.expect(!compatibleProbe(gpa, "phux 99.0.0"));
    try std.testing.expect(!compatibleProbe(gpa, "{}"));
    const unknown_schema = try std.mem.replaceOwned(u8, gpa, test_probe, "schema_version\":1", "schema_version\":2");
    defer gpa.free(unknown_schema);
    try std.testing.expect(!compatibleProbe(gpa, unknown_schema));
    const incompatible = try std.mem.replaceOwned(u8, gpa, test_probe, "minor\":9", "minor\":8");
    defer gpa.free(incompatible);
    try std.testing.expect(!compatibleProbe(gpa, incompatible));
    const missing = try std.mem.replaceOwned(u8, gpa, test_probe, "server-ensure-v1", "unsupported");
    defer gpa.free(missing);
    try std.testing.expect(!compatibleProbe(gpa, missing));
}

test "installed candidate probe is read-only with exact argv and bounded output" {
    var fixture = try TestFixture.init();
    defer fixture.deinit();
    const script = "#!/bin/sh\n[ \"$#\" = 2 ] && [ \"$1\" = runtime-info ] && [ \"$2\" = --json ] || exit 9\nprintf '%s\\n' '" ++ test_probe ++ "'\n";
    try fixture.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "fixture cli", .data = script, .flags = .{ .permissions = .fromMode(0o700) } });
    var stopping = std.atomic.Value(bool).init(false);
    try std.testing.expect(try compatibleCandidate(std.testing.allocator, std.testing.io, fixture.cli, &stopping));
    var output: Capture = .{};
    var evidence: Evidence = .{};
    try std.testing.expectError(error.HelperOutputTooLarge, runHelper(std.testing.io, &.{ "/usr/bin/yes", "unbounded" }, &stopping, 1000, &output, &evidence));
    try std.testing.expectEqual(@as(usize, 4096), output.len);
}

test "failure evidence retains helper executable socket status and stderr" {
    var fixture = try TestFixture.init();
    defer fixture.deinit();
    try fixture.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "fixture cli", .data = "#!/bin/sh\necho 'permission denied fixture' >&2\nexit 7\n", .flags = .{ .permissions = .fromMode(0o700) } });
    var stopping = std.atomic.Value(bool).init(false);
    var evidence: Evidence = .{};
    try std.testing.expectError(error.EnsureFailed, ensure(std.testing.allocator, std.testing.io, fixture.socket, &stopping, .{ .cli_path = fixture.cli, .evidence = &evidence }));
    try std.testing.expectEqualStrings(fixture.cli, evidence.cli());
    try std.testing.expectEqualStrings("permission denied fixture\n", evidence.stderr.text());
    try std.testing.expectEqual(@as(u8, 7), std.posix.W.EXITSTATUS(@intCast(evidence.status.?)));
    var status: Status = .{};
    status.record(evidence, fixture.socket, error.EnsureFailed);
    var text: [1024]u8 = undefined;
    const failure = status.failureInto(&text);
    try std.testing.expect(std.mem.indexOf(u8, failure, fixture.socket) != null);
    try std.testing.expect(std.mem.indexOf(u8, failure, "Repair Installation") != null);
}

test "existing exact socket never launches helper even when its server version is unknown" {
    var fixture = try TestFixture.init();
    defer fixture.deinit();
    var address = std.mem.zeroes(std.posix.sockaddr.un);
    address.len = @intCast(@offsetOf(std.posix.sockaddr.un, "path") + fixture.socket.len + 1);
    address.family = std.posix.AF.UNIX;
    @memcpy(address.path[0..fixture.socket.len], fixture.socket);
    const fd = std.c.socket(std.posix.AF.UNIX, std.posix.SOCK.STREAM, 0);
    try std.testing.expect(fd >= 0);
    defer _ = std.c.close(fd);
    try std.testing.expectEqual(@as(c_int, 0), std.c.bind(fd, @ptrCast(&address), address.len));
    try std.testing.expectEqual(@as(c_int, 0), std.c.listen(fd, 8));
    var stopping = std.atomic.Value(bool).init(false);
    try ensure(std.testing.allocator, std.testing.io, fixture.socket, &stopping, .{ .cli_path = "/usr/bin/false" });
    // No Hello/version assumptions are needed to leave a listening server alone.
}

test "isolated live coordinator is reused without invoking even a broken CLI" {
    const socket = environment("PHUX_COCKPIT_TEST_LOCAL_SOCKET") orelse return error.SkipZigTest;
    // This optional integration probe is only allowed in the harness scratch
    // root, never against the caller's PHUX_SOCKET or default coordinator.
    try std.testing.expect(std.mem.startsWith(u8, socket, "/private/tmp/opencode/"));
    var stopping = std.atomic.Value(bool).init(false);
    try ensure(std.testing.allocator, std.testing.io, socket, &stopping, .{ .cli_path = "/usr/bin/false" });
}

fn expectExitedHelperCleanup(code: []const u8) !void {
    var fixture = try TestFixture.init();
    defer fixture.deinit();
    const script = try std.fmt.allocPrint(std.testing.allocator, "#!/usr/bin/python3\nimport pathlib, subprocess, sys\n" ++
        "p = subprocess.Popen(['/bin/sleep', '60'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\n" ++
        "(pathlib.Path(__file__).parent / 'probe-pid').write_text(str(p.pid))\n" ++
        "sys.exit({s})\n", .{code});
    defer std.testing.allocator.free(script);
    try fixture.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "fixture cli", .data = script, .flags = .{ .permissions = .fromMode(0o700) } });
    var stopping = std.atomic.Value(bool).init(false);
    const result = ensure(std.testing.allocator, std.testing.io, fixture.socket, &stopping, .{ .cli_path = fixture.cli });
    // The assertion's failure path kills the owned fixture process as well.
    try fixture.expectProbeStopped();
    if (std.mem.eql(u8, code, "0")) try result else try std.testing.expectError(error.EnsureFailed, result);
}

test "successful exited helper cannot leave disposable process group children" {
    try expectExitedHelperCleanup("0");
}

test "failed exited helper cannot leave disposable process group children" {
    try expectExitedHelperCleanup("7");
}

test "exited helper cleanup preserves a daemon that acquired its own session" {
    var fixture = try TestFixture.init();
    defer fixture.deinit();
    const script = "#!/usr/bin/python3\nimport pathlib, subprocess\n" ++
        "p = subprocess.Popen(['/bin/sleep', '60'], start_new_session=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\n" ++
        "(pathlib.Path(__file__).parent / 'daemon-pid').write_text(str(p.pid))\n";
    try fixture.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "fixture cli", .data = script, .flags = .{ .permissions = .fromMode(0o700) } });
    var stopping = std.atomic.Value(bool).init(false);
    try ensure(std.testing.allocator, std.testing.io, fixture.socket, &stopping, .{ .cli_path = fixture.cli });
    const text = try fixture.tmp.dir.readFileAlloc(std.testing.io, "daemon-pid", std.testing.allocator, .limited(32));
    defer std.testing.allocator.free(text);
    const pid = try std.fmt.parseInt(std.posix.pid_t, text, 10);
    defer _ = std.c.kill(pid, .KILL);
    try std.testing.expectEqual(@as(c_int, 0), std.c.kill(pid, @enumFromInt(0)));
}

test "stderr overflow is bounded and retained without blocking helper cleanup" {
    var stopping = std.atomic.Value(bool).init(false);
    var stdout: Capture = .{};
    var evidence: Evidence = .{};
    try std.testing.expectError(error.HelperOutputTooLarge, runHelper(std.testing.io, &.{ "/bin/sh", "-c", "exec /usr/bin/yes fixture-error >&2" }, &stopping, 1000, &stdout, &evidence));
    try std.testing.expectEqual(@as(usize, 4096), evidence.stderr.len);
    try std.testing.expectEqual(@as(usize, 0), stdout.len);
}

test "duplicate installed candidates run only one read-only probe" {
    var fixture = try TestFixture.init();
    defer fixture.deinit();
    try fixture.release("7");
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    var candidates: Candidates = .{ .gpa = arena.allocator() };
    try candidates.add(fixture.cli);
    try candidates.add(fixture.cli);
    var stopping = std.atomic.Value(bool).init(false);
    try std.testing.expectEqual(@as(?[]u8, null), try selectCompatible(std.testing.allocator, std.testing.io, candidates.paths.items, &stopping));
    const calls = try fixture.tmp.dir.readFileAlloc(std.testing.io, "calls", std.testing.allocator, .limited(4096));
    defer std.testing.allocator.free(calls);
    try std.testing.expectEqualStrings("runtime-info\n--json\n", calls);
}
