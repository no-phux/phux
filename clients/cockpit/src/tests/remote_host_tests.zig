//! Remote hosts: the `phux-remote` setting, its environment precedence, the
//! remembered host that survives relaunch, provider selection at startup,
//! and Connect to Host through the engine's one Phux provider.

const std = @import("std");
const config = @import("../config/config.zig");
const startup = @import("../cockpit/startup.zig");
const remote_memory = @import("../cockpit/remote_memory.zig");
const remote_hosts = @import("../cockpit/native/remote_hosts.zig");
const support = @import("../cockpit/phux_support.zig");
const ts_engine = @import("../cockpit/native/ts_engine.zig");

const testing = std.testing;

test "phux-remote parses a registry label and refuses URIs, whitespace and overlong values" {
    const parsed = config.parse("phux-remote = me@mini:8788\n");
    try testing.expectEqualStrings("me@mini:8788", parsed.phux_remote.slice());
    try testing.expectEqual(config.PhuxValueSource.config, parsed.phux_remote_source);
    try testing.expectEqual(@as(usize, 0), parsed.diagnostic_count);

    for ([_][]const u8{ "phux-remote = quic://mini:8788\n", "phux-remote = me mini\n" }) |line| {
        const refused = config.parse(line);
        try testing.expectEqual(@as(usize, 0), refused.phux_remote.slice().len);
        try testing.expectEqual(@as(usize, 1), refused.diagnostic_count);
        try testing.expectEqual(config.Diagnostic.Kind.bad_value, refused.diagnosticSlice()[0].kind);
    }

    var long: [14 + config.max_phux_remote_bytes + 1]u8 = undefined;
    @memcpy(long[0..14], "phux-remote = ");
    @memset(long[14..], 'a');
    const overlong = config.parse(&long);
    try testing.expectEqual(@as(usize, 0), overlong.phux_remote.slice().len);
    try testing.expectEqual(config.Diagnostic.Kind.too_long, overlong.diagnosticSlice()[0].kind);
}

test "PHUX_REMOTE outranks the config, and an empty value is unset" {
    const parsed = config.parse("phux-remote = mini\n");
    const from_env = startup.resolvePhuxConfig(parsed, .{ .remote = "studio", .runtime_dir = "/tmp/rt" });
    try testing.expectEqualStrings("studio", from_env.phux_remote.slice());
    try testing.expectEqual(config.PhuxValueSource.environment, from_env.phux_remote_source);
    const empty_env = startup.resolvePhuxConfig(parsed, .{ .remote = "", .runtime_dir = "/tmp/rt" });
    try testing.expectEqualStrings("mini", empty_env.phux_remote.slice());
    try testing.expectEqual(config.PhuxValueSource.config, empty_env.phux_remote_source);
}

test "the remembered host is one exact line, and anything else is forgotten" {
    var out: [remote_memory.max_file_bytes]u8 = undefined;
    const encoded = remote_memory.encode("me@mini", &out).?;
    try testing.expectEqualStrings("phux-cockpit-remote v1\ntarget=me@mini\n", encoded);
    try testing.expectEqualStrings("me@mini", remote_memory.parse(encoded).?);
    for ([_][]const u8{
        "",
        "phux-cockpit-remote v1\n",
        "phux-cockpit-remote v1\ntarget=\n",
        "phux-cockpit-remote v1\ntarget=me@mini",
        "phux-cockpit-remote v1\ntarget=me@mini\nextra\n",
        "phux-cockpit-remote v2\ntarget=me@mini\n",
        "phux-cockpit-remote v1\ntarget=quic://mini:1\n",
    }) |bytes| try testing.expect(remote_memory.parse(bytes) == null);
    try testing.expect(remote_memory.encode("", &out) == null);
    try testing.expect(remote_memory.encode("a b", &out) == null);
}

test "a remembered host restores at launch unless the config names one, and returning local forgets it" {
    const io = testing.io;
    const gpa = testing.allocator;
    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();
    try tmp.dir.writeFile(io, .{ .sub_path = "anchor", .data = "" });
    const anchor = try tmp.dir.realPathFileAlloc(io, "anchor", gpa);
    defer gpa.free(anchor);
    // The state directory does not exist yet: remembering creates it.
    const state_path = try std.fs.path.join(gpa, &.{ std.fs.path.dirname(anchor).?, "State", "workspace.state" });
    defer gpa.free(state_path);
    defer _ = remote_memory.setPathFor(null);

    var absent: config.Config = .{};
    startup.restoreRememberedRemote(io, state_path, &absent);
    try testing.expectEqual(@as(usize, 0), absent.phux_remote.slice().len);

    const memory = remote_memory.setPathFor(state_path).?;
    remote_memory.store(io, memory, "me@mini");
    var restored: config.Config = .{};
    startup.restoreRememberedRemote(io, state_path, &restored);
    try testing.expectEqualStrings("me@mini", restored.phux_remote.slice());
    try testing.expectEqual(config.PhuxValueSource.default, restored.phux_remote_source);

    var explicit = config.parse("phux-remote = studio\n");
    startup.restoreRememberedRemote(io, state_path, &explicit);
    try testing.expectEqualStrings("studio", explicit.phux_remote.slice());

    remote_memory.store(io, memory, null);
    var forgotten: config.Config = .{};
    startup.restoreRememberedRemote(io, state_path, &forgotten);
    try testing.expectEqual(@as(usize, 0), forgotten.phux_remote.slice().len);
}

test "startup selects a registered remote host instead of the local socket" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const gpa = testing.allocator;
    const io = testing.io;
    var remote_config = startup.resolvePhuxConfig(config.parse("phux-remote = me@mini\n"), .{ .runtime_dir = "/tmp/rt" });
    const remote = (try startup.createPhuxProviderFromConfig(gpa, io, &remote_config)).?;
    defer remote.destroy();
    try testing.expectEqualStrings("me@mini", remote.endpointDescriptor().remote.target);
    try testing.expectEqualStrings("me@mini", startup.configuredPhuxRemote(remote).?);
    try testing.expectEqualStrings("", startup.configuredPhuxSocket(remote));

    var local_config = startup.resolvePhuxConfig(config.parse(""), .{ .runtime_dir = "/tmp/rt" });
    const local = (try startup.createPhuxProviderFromConfig(gpa, io, &local_config)).?;
    defer local.destroy();
    try testing.expect(startup.configuredPhuxRemote(local) == null);
    try testing.expectEqualStrings("/tmp/rt/phux/phux.sock", startup.configuredPhuxSocket(local));
}

extern "c" fn setenv(name: [*:0]const u8, value: [*:0]const u8, overwrite: c_int) c_int;
extern "c" fn unsetenv(name: [*:0]const u8) c_int;

/// Point the phux CLI registry at a disposable `config.toml` for one test.
/// The tunnel reads `$XDG_CONFIG_HOME/phux/config.toml`, the CLI's own path.
const IsolatedRegistry = struct {
    tmp: testing.TmpDir,
    previous: ?[:0]u8,

    fn init(body: []const u8) !IsolatedRegistry {
        const io = testing.io;
        const gpa = testing.allocator;
        var tmp = testing.tmpDir(.{});
        errdefer tmp.cleanup();
        try tmp.dir.createDirPath(io, "phux");
        try tmp.dir.writeFile(io, .{ .sub_path = "phux/config.toml", .data = body });
        const file = try tmp.dir.realPathFileAlloc(io, "phux/config.toml", gpa);
        defer gpa.free(file);
        const root = try gpa.dupeZ(u8, std.fs.path.dirname(std.fs.path.dirname(file).?).?);
        defer gpa.free(root);
        const previous: ?[:0]u8 = if (std.c.getenv("XDG_CONFIG_HOME")) |value| try gpa.dupeZ(u8, std.mem.span(value)) else null;
        errdefer if (previous) |value| gpa.free(value);
        if (setenv("XDG_CONFIG_HOME", root, 1) != 0) return error.SetEnvFailed;
        return .{ .tmp = tmp, .previous = previous };
    }

    fn deinit(self: *IsolatedRegistry) void {
        if (self.previous) |value| {
            _ = setenv("XDG_CONFIG_HOME", value, 1);
            testing.allocator.free(value);
        } else {
            _ = unsetenv("XDG_CONFIG_HOME");
        }
        self.tmp.cleanup();
    }
};

test "Connect to Host resolves before touching the connection, then retargets the one provider" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const gpa = testing.allocator;
    const io = testing.io;
    var registry = try IsolatedRegistry.init("[[remote]]\nname = \"mini\"\nendpoint = \"ws://127.0.0.1:1\"\nsession = \"work\"\n");
    defer registry.deinit();

    const engine = try ts_engine.Engine.create(gpa, io);
    defer engine.destroy();
    const provider = try support.PhuxProvider.create(gpa, io, .{ .unix = "/unused" }, null, "remote-hosts-test");
    engine.model.phux_provider = provider;
    const fx = ts_engine.NoShells{};
    var out: [remote_hosts.max_bytes]u8 = undefined;

    // The local coordinator is not a remote host.
    try testing.expectEqualSlices(u8, "\x01\x00\x00\x00", try remote_hosts.handle(engine, &fx, "\x01\x01\x00", &out));

    // An unregistered host fails with the pairing command and changes nothing.
    const refused = try remote_hosts.handle(engine, &fx, "\x01\x02\x06studio", &out);
    try testing.expectEqual(@intFromEnum(remote_hosts.Phase.failed), refused[1]);
    try testing.expect(std.mem.indexOf(u8, refused, "phux --remote studio") != null);
    try testing.expect(provider.remoteTarget() == null);
    try testing.expectEqualStrings("/unused", provider.endpointDescriptor().unix);

    // A registered one retargets, labelled by its registry entry.
    const accepted = try remote_hosts.handle(engine, &fx, "\x01\x02\x07me@mini", &out);
    try testing.expectEqualSlices(u8, "\x01\x01\x04mini\x00", accepted);
    try testing.expectEqualStrings("me@mini", provider.remoteTarget().?);
    try testing.expectEqualStrings("mini", provider.remoteLabel().?);
    try testing.expectEqualSlices(u8, "\x01\x01\x04mini\x00", try remote_hosts.handle(engine, &fx, "\x01\x01\x00", &out));

    // Use this Mac: back to the configured socket, no host named.
    try testing.expectEqualSlices(u8, "\x01\x00\x00\x00", try remote_hosts.handle(engine, &fx, "\x01\x03\x00", &out));
    try testing.expect(provider.remoteTarget() == null);

    try testing.expectError(error.InvalidRequest, remote_hosts.handle(engine, &fx, "\x01\x02\x03a b", &out));
}

test "only a host chosen through Connect to Host is remembered, never one the environment selected" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const gpa = testing.allocator;
    const io = testing.io;
    var registry = try IsolatedRegistry.init("[[remote]]\nname = \"mini\"\nendpoint = \"ws://127.0.0.1:1\"\n");
    defer registry.deinit();
    try registry.tmp.dir.writeFile(io, .{ .sub_path = "anchor", .data = "" });
    const anchor = try registry.tmp.dir.realPathFileAlloc(io, "anchor", gpa);
    defer gpa.free(anchor);
    const state_path = try std.fs.path.join(gpa, &.{ std.fs.path.dirname(anchor).?, "workspace.state" });
    defer gpa.free(state_path);
    const memory = remote_memory.setPathFor(state_path).?;
    defer _ = remote_memory.setPathFor(null);
    remote_hosts.forgetForTests();
    defer remote_hosts.forgetForTests();

    const engine = try ts_engine.Engine.create(gpa, io);
    defer engine.destroy();
    // Exactly what `PHUX_REMOTE=studio` builds at launch.
    const provider = try support.PhuxProvider.create(gpa, io, .{ .remote = .{ .target = "studio" } }, null, "remember-test");
    engine.model.phux_provider = provider;
    try support.PhuxProvider.test_support.attachHost(provider.host);
    const fx = ts_engine.NoShells{};
    var out: [remote_hosts.max_bytes]u8 = undefined;
    var remembered: [config.max_phux_remote_bytes]u8 = undefined;

    const selected = try remote_hosts.handle(engine, &fx, "\x01\x01\x00", &out);
    try testing.expectEqual(@intFromEnum(remote_hosts.Phase.connected), selected[1]);
    try testing.expect(remote_memory.load(io, memory, &remembered) == null);

    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x07me@mini", &out);
    const chosen = try remote_hosts.handle(engine, &fx, "\x01\x01\x00", &out);
    try testing.expectEqual(@intFromEnum(remote_hosts.Phase.connected), chosen[1]);
    try testing.expectEqualStrings("me@mini", remote_memory.load(io, memory, &remembered).?);
}
