//! One file catches everything the process says about itself.
//!
//! Launch Services starts the app with descriptor 2 on `/dev/null`, so
//! `std.log` lines, the segfault handler's report, a Rust panic message and
//! the Phux bridge's tracing all had nowhere to go; a crash left Apple's
//! `.ips` report and nothing about what the app was doing. `install` opens
//! the log, points descriptor 2 at it when nothing else is listening, prints
//! one banner, and asks the bridge to log through the same descriptor.
//!
//! Descriptor 2 is left alone when something already reads it — a terminal,
//! or the file `dev-run.sh` redirects into — so developers keep seeing output
//! where they expect it. `PHUX_COCKPIT_LOG` names the file and also forces
//! the redirect: naming a file is asking for it to be written.
//!
//! Logging is diagnostics, never a gate: every failure here is one warning
//! and the app starts anyway.

const std = @import("std");
const builtin = @import("builtin");
const native_sdk = @import("native_sdk");
const phux_options = @import("phux_options");
const provider = @import("phux_provider");
const config_module = @import("../config/config.zig");
const scene = @import("native/scene.zig");

const app_name = scene.app_name;

/// The file under the platform's log directory for this app (macOS:
/// `~/Library/Logs/Phux Cockpit/cockpit.log`).
pub const file_name = "cockpit.log";
/// Where the previous log goes when the current one is rotated at startup.
pub const rotated_suffix = ".1";
/// Names the log file directly and forces the redirect.
pub const path_env = "PHUX_COCKPIT_LOG";
/// A RUST_LOG-style directive list for the Phux bridge; unset means the
/// bridge default.
pub const filter_env = "PHUX_COCKPIT_LOG_FILTER";
/// A log larger than this at startup is rotated aside before it is reopened.
pub const rotate_bytes: u64 = 8 * 1024 * 1024;

/// Where the log is written, resolved through the SDK's `app_dirs` primitive
/// so the platform owns the rule. An explicit override names a file directly
/// and wins; an empty override counts as unset by the caller's contract.
/// Null means no directory could be resolved, which leaves stderr alone.
pub fn resolveLogPath(
    env: native_sdk.app_dirs.Env,
    override_path: ?[]const u8,
    dir_storage: []u8,
    path_storage: []u8,
) ?[]const u8 {
    if (override_path) |explicit| {
        if (explicit.len == 0 or explicit.len > path_storage.len) return null;
        @memcpy(path_storage[0..explicit.len], explicit);
        return path_storage[0..explicit.len];
    }
    const dir = native_sdk.app_dirs.resolveOne(
        .{ .name = app_name },
        native_sdk.app_dirs.currentPlatform(),
        env,
        .logs,
        dir_storage,
    ) catch return null;
    return config_module.joinDir(dir, file_name, path_storage) catch null;
}

/// `<path>.1`, or null when it does not fit.
pub fn rotatedPath(path: []const u8, storage: []u8) ?[]const u8 {
    const total = path.len + rotated_suffix.len;
    if (total > storage.len) return null;
    @memcpy(storage[0..path.len], path);
    @memcpy(storage[path.len..][0..rotated_suffix.len], rotated_suffix);
    return storage[0..total];
}

/// What `fstat` says about a descriptor, reduced to the two facts the
/// redirect decision needs.
pub const Probe = struct {
    char_device: bool,
    rdev: i64,
};

/// Redirect when descriptor 2 is the null device — the Launch Services case
/// — or when the operator named a file. A terminal (a character device with
/// some other `rdev`), a pipe, or a regular file is someone already
/// listening, so it stays. An unprobeable descriptor stays too: guessing
/// wrong would steal output from a listener.
pub fn shouldRedirect(stderr: ?Probe, dev_null: ?Probe, explicit: bool) bool {
    if (explicit) return true;
    const fd = stderr orelse return false;
    const null_device = dev_null orelse return false;
    return fd.char_device and null_device.char_device and fd.rdev == null_device.rdev;
}

/// Rotate a log that has outgrown the ceiling. The check runs once per
/// launch, so a single very chatty session can exceed it; the next launch
/// rotates it aside.
pub fn shouldRotate(size: u64) bool {
    return size > rotate_bytes;
}

/// Open the log, route descriptor 2 into it when appropriate, print the
/// banner, and route the Phux bridge's tracing to the same descriptor.
pub fn install(init: std.process.Init) void {
    const io = init.io;
    var dir_storage: [std.fs.max_path_bytes]u8 = undefined;
    var path_storage: [std.fs.max_path_bytes]u8 = undefined;
    const env = native_sdk.debug.envFromMap(init.environ_map);
    const override = nonEmpty(init.environ_map.get(path_env));
    const path = resolveLogPath(env, override, &dir_storage, &path_storage) orelse {
        std.log.warn("diagnostics: no log directory could be resolved; stderr stays where it is", .{});
        return;
    };

    var redirected = false;
    if (shouldRedirect(probeFd(std.posix.STDERR_FILENO), probePath("/dev/null"), override != null)) {
        if (redirectStderr(io, path)) {
            redirected = true;
        } else |err| {
            std.log.warn("diagnostics: could not route stderr to {s}: {s}", .{ path, @errorName(err) });
        }
    }

    std.log.info("{s} pid {d} ({s} build) logging to {s}", .{
        app_name,
        std.c.getpid(),
        @tagName(builtin.mode),
        if (redirected) path else "the inherited stderr",
    });

    if (comptime phux_options.enabled) {
        const filter = init.environ_map.get(filter_env) orelse "";
        provider.logInit(filter) catch |err| {
            std.log.warn("diagnostics: Phux bridge logging is off: {s}", .{@errorName(err)});
        };
    }
}

fn nonEmpty(value: ?[]const u8) ?[]const u8 {
    const text = value orelse return null;
    return if (text.len == 0) null else text;
}

fn probeOf(stat: std.c.Stat) Probe {
    return .{
        .char_device = (stat.mode & std.c.S.IFMT) == std.c.S.IFCHR,
        .rdev = @intCast(stat.rdev),
    };
}

fn probeFd(fd: std.posix.fd_t) ?Probe {
    var stat: std.c.Stat = undefined;
    if (std.c.fstat(fd, &stat) != 0) return null;
    return probeOf(stat);
}

/// Probe a path through a short-lived descriptor: `fstat` is the one stat
/// entry point libc exposes uniformly across Apple architectures.
fn probePath(path: [*:0]const u8) ?Probe {
    const fd = std.c.open(path, .{ .ACCMODE = .RDONLY, .CLOEXEC = true });
    if (fd < 0) return null;
    defer _ = std.c.close(fd);
    return probeFd(fd);
}

const RedirectError = error{
    PathTooLong,
    OpenFailed,
    DupFailed,
} || std.Io.Dir.CreateDirPathOpenError;

/// Create the directory (owner-only), rotate an oversized log aside, open
/// the file append-only at mode 0600, and make it descriptor 2. `dup2`
/// shares the open file description, so the append flag rides along and
/// every writer — Zig, Rust, the signal handler — lands after the last line.
fn redirectStderr(io: std.Io, path: []const u8) RedirectError!void {
    const cwd = std.Io.Dir.cwd();
    if (std.fs.path.dirname(path)) |dir| {
        var opened = try cwd.createDirPathOpen(io, dir, .{ .permissions = @enumFromInt(0o700) });
        opened.close(io);
    }

    if (cwd.statFile(io, path, .{})) |stat| {
        if (shouldRotate(stat.size)) {
            var rotated_storage: [std.fs.max_path_bytes]u8 = undefined;
            if (rotatedPath(path, &rotated_storage)) |rotated| {
                cwd.rename(path, cwd, rotated, io) catch |err| {
                    std.log.warn("diagnostics: could not rotate {s}: {s}", .{ path, @errorName(err) });
                };
            }
        }
    } else |_| {}

    if (path.len >= std.fs.max_path_bytes) return error.PathTooLong;
    var path_z: [std.fs.max_path_bytes:0]u8 = undefined;
    @memcpy(path_z[0..path.len], path);
    path_z[path.len] = 0;
    const fd = std.c.open(
        path_z[0..path.len :0],
        .{ .ACCMODE = .WRONLY, .CREAT = true, .APPEND = true, .CLOEXEC = true },
        @as(c_uint, 0o600),
    );
    if (fd < 0) return error.OpenFailed;
    defer _ = std.c.close(fd);
    if (std.c.dup2(fd, std.posix.STDERR_FILENO) < 0) return error.DupFailed;
}

test "the log path is the app-dirs logs directory, and the env override wins" {
    var dir_storage: [512]u8 = undefined;
    var path_storage: [512]u8 = undefined;
    try std.testing.expectEqual(
        @as(?[]const u8, null),
        resolveLogPath(.{}, null, &dir_storage, &path_storage),
    );

    const explicit = resolveLogPath(
        .{ .home = "/Users/alice" },
        "/tmp/somewhere/cockpit.log",
        &dir_storage,
        &path_storage,
    ) orelse return error.TestExpectedPath;
    try std.testing.expectEqualStrings("/tmp/somewhere/cockpit.log", explicit);

    try std.testing.expectEqual(
        @as(?[]const u8, null),
        resolveLogPath(.{ .home = "/Users/alice" }, "", &dir_storage, &path_storage),
    );

    const resolved = resolveLogPath(
        .{ .home = "/Users/alice", .xdg_state_home = "/Users/alice/.local/state" },
        null,
        &dir_storage,
        &path_storage,
    ) orelse return error.TestExpectedPath;
    try std.testing.expect(std.mem.startsWith(u8, resolved, "/Users/alice/"));
    try std.testing.expect(std.mem.endsWith(u8, resolved, "/cockpit.log"));
    if (native_sdk.app_dirs.currentPlatform() == .macos) {
        try std.testing.expectEqualStrings("/Users/alice/Library/Logs/Phux Cockpit/cockpit.log", resolved);
    }
}

test "the rotated name is the log name plus .1" {
    var storage: [64]u8 = undefined;
    try std.testing.expectEqualStrings(
        "/var/log/cockpit.log.1",
        rotatedPath("/var/log/cockpit.log", &storage) orelse return error.TestExpectedPath,
    );
    var tiny: [4]u8 = undefined;
    try std.testing.expectEqual(@as(?[]const u8, null), rotatedPath("/var/log/cockpit.log", &tiny));
}

test "stderr is redirected only from the null device or on explicit request" {
    const null_device: Probe = .{ .char_device = true, .rdev = 0x3000002 };
    const tty: Probe = .{ .char_device = true, .rdev = 0x10000004 };
    const regular_file: Probe = .{ .char_device = false, .rdev = 0 };

    try std.testing.expect(shouldRedirect(null_device, null_device, false));
    try std.testing.expect(!shouldRedirect(tty, null_device, false));
    try std.testing.expect(!shouldRedirect(regular_file, null_device, false));
    try std.testing.expect(!shouldRedirect(null, null_device, false));
    try std.testing.expect(!shouldRedirect(null_device, null, false));
    try std.testing.expect(shouldRedirect(tty, null_device, true));
    try std.testing.expect(shouldRedirect(null, null, true));
}

test "rotation triggers strictly above the ceiling" {
    try std.testing.expect(!shouldRotate(0));
    try std.testing.expect(!shouldRotate(rotate_bytes));
    try std.testing.expect(shouldRotate(rotate_bytes + 1));
}
