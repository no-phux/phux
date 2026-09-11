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

test "remembered hosts are a short exact list, and the one-host file still reads" {
    var hosts: remote_memory.Hosts = .{};
    try testing.expect(hosts.add("me@mini"));
    try testing.expect(hosts.add("studio"));
    try testing.expect(!hosts.add("studio"));
    try testing.expect(!hosts.add("a b"));
    var out: [remote_memory.max_list_file_bytes]u8 = undefined;
    const encoded = remote_memory.encodeAll(&hosts, &out).?;
    try testing.expectEqualStrings("phux-cockpit-remote v2\ntarget=me@mini\ntarget=studio\n", encoded);
    var parsed: remote_memory.Hosts = .{};
    try testing.expect(remote_memory.parseAll(encoded, &parsed));
    try testing.expectEqual(@as(usize, 2), parsed.count);
    try testing.expectEqualStrings("me@mini", parsed.get(0));
    try testing.expectEqualStrings("studio", parsed.get(1));
    // A file an earlier release wrote reads as one host.
    try testing.expect(remote_memory.parseAll("phux-cockpit-remote v1\ntarget=me@mini\n", &parsed));
    try testing.expectEqual(@as(usize, 1), parsed.count);
    try testing.expectEqualStrings("me@mini", parsed.get(0));
    // A full list takes no more; removing one keeps the others in order.
    try testing.expect(hosts.add("lab"));
    try testing.expect(!hosts.add("rack"));
    try testing.expect(hosts.remove("me@mini"));
    try testing.expect(!hosts.remove("me@mini"));
    try testing.expectEqual(@as(usize, 2), hosts.count);
    try testing.expectEqualStrings("studio", hosts.get(0));
    try testing.expectEqualStrings("lab", hosts.get(1));
    for ([_][]const u8{
        "phux-cockpit-remote v2\n",
        "phux-cockpit-remote v2\ntarget=mini\ntarget=mini\n",
        "phux-cockpit-remote v2\ntarget=a\ntarget=b\ntarget=c\ntarget=d\n",
        "phux-cockpit-remote v2\ntarget=mini",
        "phux-cockpit-remote v2\ntarget=mini\nextra\n",
        "phux-cockpit-remote v2\ntarget=\n",
        "phux-cockpit-remote v3\ntarget=mini\n",
    }) |bytes| {
        try testing.expect(!remote_memory.parseAll(bytes, &parsed));
        try testing.expectEqual(@as(usize, 0), parsed.count);
    }
    try testing.expect(remote_memory.encodeAll(&remote_memory.Hosts{}, &out) == null);
}

test "a remembered host's record rides on the line after its target, and a malformed record forgets every host" {
    // ADR-0110: what a host showed is kept beside that host, never apart.
    var hosts: remote_memory.Hosts = .{};
    try testing.expect(hosts.add("me@mini"));
    try testing.expect(hosts.add("studio"));
    const front: remote_memory.Shown = .{ .session = 42, .server = 0x0123_4567_89ab_cdef, .window = @splat(0xab), .front = true };
    const behind: remote_memory.Shown = .{ .session = 3, .server = 0x10 };
    try testing.expect(hosts.setShown(0, front));
    try testing.expect(!hosts.setShown(0, front));
    var out: [remote_memory.max_list_file_bytes]u8 = undefined;
    const encoded = remote_memory.encodeAll(&hosts, &out).?;
    try testing.expectEqualStrings("phux-cockpit-remote v3\ntarget=me@mini\nshown=42,0123456789abcdef," ++ "ab" ** 16 ++ ",1\ntarget=studio\n", encoded);
    var parsed: remote_memory.Hosts = .{};
    try testing.expect(remote_memory.parseAll(encoded, &parsed));
    try testing.expectEqual(@as(usize, 2), parsed.count);
    try testing.expect(parsed.shown[0].?.eql(front));
    try testing.expect(parsed.shown[1] == null);

    // Without a record the file stays the v2 list earlier releases read.
    try testing.expect(hosts.setShown(0, null));
    try testing.expectEqualStrings("phux-cockpit-remote v2\ntarget=me@mini\ntarget=studio\n", remote_memory.encodeAll(&hosts, &out).?);

    // Removing a host removes its record; the other keeps its own.
    try testing.expect(hosts.setShown(0, front));
    try testing.expect(hosts.setShown(1, behind));
    try testing.expect(hosts.remove("me@mini"));
    try testing.expectEqual(@as(usize, 1), hosts.count);
    try testing.expectEqualStrings("studio", hosts.get(0));
    try testing.expect(hosts.shown[0].?.eql(behind));
    try testing.expect(hosts.shown[1] == null);
    try testing.expectEqualStrings("phux-cockpit-remote v3\ntarget=studio\nshown=3,0000000000000010,-,0\n", remote_memory.encodeAll(&hosts, &out).?);

    for ([_][]const u8{
        // v3 is written only with a record; a record only in v3.
        "phux-cockpit-remote v3\ntarget=mini\n",
        "phux-cockpit-remote v2\ntarget=mini\nshown=1,0000000000000001,-,1\n",
        // Only directly after its host, and at most one per host.
        "phux-cockpit-remote v3\nshown=1,0000000000000001,-,1\ntarget=mini\n",
        "phux-cockpit-remote v3\ntarget=mini\nshown=1,0000000000000001,-,1\nshown=2,0000000000000001,-,0\n",
        // Exact fields.
        "phux-cockpit-remote v3\ntarget=mini\nshown=0,0000000000000001,-,1\n",
        "phux-cockpit-remote v3\ntarget=mini\nshown=01,0000000000000001,-,1\n",
        "phux-cockpit-remote v3\ntarget=mini\nshown=1,001,-,1\n",
        "phux-cockpit-remote v3\ntarget=mini\nshown=1,0000_00000000001,-,1\n",
        "phux-cockpit-remote v3\ntarget=mini\nshown=1,0000000000000001,abc,1\n",
        "phux-cockpit-remote v3\ntarget=mini\nshown=1,0000000000000001,-,2\n",
        "phux-cockpit-remote v3\ntarget=mini\nshown=1,0000000000000001,-,1,x\n",
        "phux-cockpit-remote v3\ntarget=mini\nshown=1,0000000000000001,-,1",
    }) |bytes| {
        try testing.expect(!remote_memory.parseAll(bytes, &parsed));
        try testing.expectEqual(@as(usize, 0), parsed.count);
    }
}

test "a file naming two front records keeps every host and only the first front" {
    // Review of phux-c2td.30: rejecting such a file forgot every host.
    var parsed: remote_memory.Hosts = .{};
    try testing.expect(remote_memory.parseAll("phux-cockpit-remote v3\ntarget=mini\nshown=1,0000000000000001,-,1\ntarget=studio\nshown=2,0000000000000001,-,1\n", &parsed));
    try testing.expectEqual(@as(usize, 2), parsed.count);
    try testing.expectEqualStrings("mini", parsed.get(0));
    try testing.expectEqualStrings("studio", parsed.get(1));
    try testing.expect(parsed.shown[0].?.front);
    try testing.expect(!parsed.shown[1].?.front);
    try testing.expectEqual(@as(u32, 2), parsed.shown[1].?.session);
}

test "a record names its session by id and creation time, and a record kept with a server hash still reads" {
    // phux-c2td.32: a server hash dropped the record on a graceful upgrade,
    // which changes HELLO_OK.server_id but keeps sessions and their times.
    var hosts: remote_memory.Hosts = .{};
    try testing.expect(hosts.add("mini"));
    const shown: remote_memory.Shown = .{ .session = 7, .created = 1_757_000_000, .window = @splat(0xcd), .front = true };
    try testing.expect(hosts.setShown(0, shown));
    var out: [remote_memory.max_list_file_bytes]u8 = undefined;
    const encoded = remote_memory.encodeAll(&hosts, &out).?;
    try testing.expectEqualStrings("phux-cockpit-remote v3\ntarget=mini\nshown=7,@1757000000," ++ "cd" ** 16 ++ ",1\n", encoded);
    var parsed: remote_memory.Hosts = .{};
    try testing.expect(remote_memory.parseAll(encoded, &parsed));
    try testing.expect(parsed.shown[0].?.eql(shown));
    // The same id created at another second is another session.
    try testing.expect(!shown.eql(.{ .session = 7, .created = 1_757_000_001, .window = @splat(0xcd), .front = true }));
    // Every creation time an i64 holds round-trips, a clock before 1970 included.
    for ([_]i64{ 0, -5, std.math.maxInt(i64), std.math.minInt(i64) }) |created| {
        const value: remote_memory.Shown = .{ .session = 1, .created = created };
        try testing.expect(hosts.setShown(0, value));
        try testing.expect(remote_memory.parseAll(remote_memory.encodeAll(&hosts, &out).?, &parsed));
        try testing.expect(parsed.shown[0].?.eql(value));
    }

    // A line written before creation times were kept still reads as it did.
    try testing.expect(remote_memory.parseAll("phux-cockpit-remote v3\ntarget=mini\nshown=7,0123456789abcdef,-,1\n", &parsed));
    try testing.expect(parsed.shown[0].?.created == null);
    try testing.expectEqual(@as(u64, 0x0123_4567_89ab_cdef), parsed.shown[0].?.server);
    try testing.expect(parsed.shown[0].?.front);

    // A malformed creation time forgets every host, as any malformed record does.
    for ([_][]const u8{ "@", "@-", "@01", "@-0", "@+1", "@1x", "@ 1", "@99999999999999999999" }) |field| {
        var file: [128]u8 = undefined;
        const bytes = try std.fmt.bufPrint(&file, "phux-cockpit-remote v3\ntarget=mini\nshown=7,{s},-,1\n", .{field});
        try testing.expect(!remote_memory.parseAll(bytes, &parsed));
        try testing.expectEqual(@as(usize, 0), parsed.count);
    }
}

test "the remembered hosts file is written only when its bytes change" {
    const io = testing.io;
    const gpa = testing.allocator;
    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();
    try tmp.dir.writeFile(io, .{ .sub_path = "anchor", .data = "" });
    const anchor = try tmp.dir.realPathFileAlloc(io, "anchor", gpa);
    defer gpa.free(anchor);
    const file = try std.fs.path.join(gpa, &.{ std.fs.path.dirname(anchor).?, "workspace.state.remote" });
    defer gpa.free(file);
    var hosts: remote_memory.Hosts = .{};
    try testing.expect(hosts.add("me@mini"));
    try testing.expect(remote_memory.save(io, file, &hosts));
    // Unchanged: nothing is written.
    try testing.expect(!remote_memory.save(io, file, &hosts));
    try testing.expect(hosts.setShown(0, .{ .session = 2, .server = 3, .front = true }));
    try testing.expect(remote_memory.save(io, file, &hosts));
    try testing.expect(!remote_memory.save(io, file, &hosts));
    var loaded: remote_memory.Hosts = .{};
    remote_memory.loadAll(io, file, &loaded);
    try testing.expectEqual(@as(usize, 1), loaded.count);
    try testing.expect(loaded.shown[0].?.eql(hosts.shown[0].?));
    // Nothing to remember: the file goes, once.
    const empty: remote_memory.Hosts = .{};
    try testing.expect(remote_memory.save(io, file, &empty));
    try testing.expect(!remote_memory.save(io, file, &empty));
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

test "a host selected at launch honors its registry entry's pinned session and name" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const gpa = testing.allocator;
    const io = testing.io;
    var registry = try IsolatedRegistry.init("[[remote]]\nname = \"mini\"\nendpoint = \"ws://127.0.0.1:1\"\nsession = \"work\"\n");
    defer registry.deinit();

    // What a remembered `me@mini` restores to: a host and no session. Connect
    // to Host would attach `work`; relaunch must attach the same session.
    var remembered = startup.resolvePhuxConfig(config.parse("phux-remote = me@mini\n"), .{ .runtime_dir = "/tmp/rt" });
    const pinned = (try startup.createPhuxProviderFromConfig(gpa, io, &remembered)).?;
    defer pinned.destroy();
    try testing.expectEqualStrings("work", startup.configuredPhuxSession(pinned).?);
    try testing.expectEqualStrings("mini", pinned.remoteLabel().?);
    try testing.expectEqualStrings("me@mini", pinned.remoteTarget().?);

    // A session named explicitly still wins over the pin, as on `phux --remote`.
    var explicit = startup.resolvePhuxConfig(config.parse("phux-remote = mini\n"), .{ .runtime_dir = "/tmp/rt", .session = "mine" });
    const named = (try startup.createPhuxProviderFromConfig(gpa, io, &explicit)).?;
    defer named.destroy();
    try testing.expectEqualStrings("mine", startup.configuredPhuxSession(named).?);

    // An unregistered host keeps its typed name and the server's own choice;
    // the dial reports the pairing command.
    var unknown = startup.resolvePhuxConfig(config.parse("phux-remote = studio\n"), .{ .runtime_dir = "/tmp/rt" });
    const unregistered = (try startup.createPhuxProviderFromConfig(gpa, io, &unknown)).?;
    defer unregistered.destroy();
    try testing.expect(startup.configuredPhuxSession(unregistered) == null);
    try testing.expectEqualStrings("studio", unregistered.remoteLabel().?);
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

test "every host connected through Connect to Host is remembered; Disconnect forgets one, Disconnect All every one" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const gpa = testing.allocator;
    const io = testing.io;
    var registry = try IsolatedRegistry.init("[[remote]]\nname = \"mini\"\nendpoint = \"ws://127.0.0.1:1\"\n[[remote]]\nname = \"studio\"\nendpoint = \"ws://127.0.0.1:2\"\n");
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
    const provider = try support.PhuxProvider.create(gpa, io, .{ .unix = "/remember-unused" }, null, "remember-test");
    engine.model.phux_provider = provider;
    try support.PhuxProvider.test_support.attachHost(provider.host);
    const fx = ts_engine.NoShells{};
    var out: [remote_hosts.max_bytes]u8 = undefined;
    var hosts: remote_memory.Hosts = .{};

    // Each host is remembered once a status poll sees it connected, beside
    // the ones remembered before.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x07me@mini", &out);
    _ = try remote_hosts.handle(engine, &fx, "\x01\x01\x00", &out);
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x06studio", &out);
    _ = try remote_hosts.handle(engine, &fx, "\x01\x01\x00", &out);
    remote_memory.loadAll(io, memory, &hosts);
    try testing.expectEqual(@as(usize, 2), hosts.count);
    try testing.expectEqualStrings("me@mini", hosts.get(0));
    try testing.expectEqualStrings("studio", hosts.get(1));

    // Disconnect mini, by its registry name: only it is forgotten.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x04\x04mini", &out);
    remote_memory.loadAll(io, memory, &hosts);
    try testing.expectEqual(@as(usize, 1), hosts.count);
    try testing.expectEqualStrings("studio", hosts.get(0));

    // Disconnect All: none is, and the file is gone.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x04\x00", &out);
    remote_memory.loadAll(io, memory, &hosts);
    try testing.expectEqual(@as(usize, 0), hosts.count);
    try testing.expectError(error.FileNotFound, std.Io.Dir.cwd().openFile(io, memory, .{}));
}
