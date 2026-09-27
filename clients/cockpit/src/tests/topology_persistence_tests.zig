const std = @import("std");
const app = @import("../native_test_root.zig");
const support = @import("support.zig");

const testing = std.testing;

const createDefaultSession = support.createDefaultSession;
const topology = @import("../cockpit/topology.zig");
const contract = @import("provider_contract");

fn remoteRef(id: u32) !contract.TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = try contract.RemoteResourceId.fromPhux(0, id, "") } };
}

test "v5 mixed attachments round trip without allocating remote shells" {
    const session = try createDefaultSession();
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    try model.setAttachmentContext("/tmp/coordinator a.sock", "server\x00incarnation", 71);
    const remote = try remoteRef(72);
    const tree = model.tree(0).?;
    _ = try tree.split(tree.focus, .vertical, remote);
    tree.setFraction(tree.root, 0.63);
    const satellite: contract.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{
        .phux = try contract.RemoteResourceId.fromPhux(1, 72, "h" ** 255),
    } };
    try testing.expect(model.admitTab(satellite));

    const snapshot = try model.topologySnapshot();
    try testing.expectEqual(@as(u8, 2), snapshot.tab_count);
    try testing.expectEqual(@as(u8, 2), snapshot.references.count);
    var bytes: [app.max_state_bytes]u8 = undefined;
    const encoded = try app.serializeWorkspaceState(&snapshot, &bytes);
    var parsed: app.PersistedTopologySnapshot = undefined;
    try testing.expect(app.parseWorkspaceState(encoded, &parsed));
    try testing.expectEqualDeep(snapshot, try app.migrateTopologySnapshot(parsed));
    var restored = try app.restoreModel(testing.allocator, testing.io, parsed);
    defer app.deinitModel(&restored);
    try testing.expectEqual(@as(usize, 1), restored.provider.liveShellCount());
    try testing.expect(restored.tree(0).?.find(remote) != null);
    try testing.expect(restored.tree(1).?.find(satellite) != null);
    try testing.expectEqualDeep(snapshot, try restored.topologySnapshot());
}

test "pending restored attachments survive normalization without live readiness" {
    const session = try createDefaultSession();
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    const remote = try remoteRef(72);
    try model.setAttachmentContext("/tmp/a.sock", "server-a", 71);
    const tree = model.tree(0).?;
    _ = try tree.split(tree.focus, .horizontal, remote);
    const snapshot = try model.topologySnapshot();
    var restored = try app.restoreModel(testing.allocator, testing.io, .{ .v5 = snapshot });
    defer app.deinitModel(&restored);
    restored.normalizeTopology();
    try testing.expect(restored.tree(0).?.find(remote) != null);
    try testing.expect(restored.attachmentPending(remote));
    try testing.expect(!restored.containsTerminal(remote));
    try testing.expect(restored.terminalOwner(remote) == null);
    try testing.expect(restored.remotePresentation(remote) == null);
    try testing.expect(restored.selectedTerminalRef() == null);
    var refs: [32]contract.TerminalRef = undefined;
    try testing.expectEqual(@as(usize, 1), restored.pendingRestoredRefs(&refs));
    try testing.expect(refs[0].eql(remote));
    try testing.expectEqualDeep(snapshot, try restored.topologySnapshot());
}

test "restored contexts fence reused IDs and never infer satellite incarnation" {
    const session = try createDefaultSession();
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    const remote = try remoteRef(72);
    try model.setAttachmentContext("/tmp/a.sock", "server-a", 71);
    try testing.expect(model.admitTab(remote));
    const satellite: contract.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{
        .phux = try contract.RemoteResourceId.fromPhux(1, 72, "satellite"),
    } };
    try testing.expect(model.admitTab(satellite));
    const snapshot = try model.topologySnapshot();
    var restored = try app.restoreModel(testing.allocator, testing.io, .{ .v5 = snapshot });
    defer app.deinitModel(&restored);
    try testing.expect(!restored.restoredAttachmentMatches(remote));
    const evidence = restored.restoredAttachmentContext(remote).?;
    try testing.expectEqual(@as(u32, 71), evidence.session_id);
    try testing.expectEqualStrings("server-a", evidence.server_id.slice());
    try restored.setAttachmentContext("/tmp/b.sock", "server-a", 71);
    try testing.expect(!restored.restoredAttachmentMatches(remote));
    try restored.setAttachmentContext("/tmp/a.sock", "server-b", 71);
    try testing.expect(!restored.restoredAttachmentMatches(remote));
    try restored.setAttachmentContext("/tmp/a.sock", "server-a", 72);
    try testing.expect(!restored.restoredAttachmentMatches(remote));
    try restored.setAttachmentContext("/tmp/a.sock", "server-a", 71);
    try testing.expect(restored.restoredAttachmentMatches(remote));
    try testing.expect(!restored.restoredAttachmentMatches(satellite));
    try testing.expect(!restored.resolveRestoredAttachment(remote));
    try testing.expect(restored.attachmentPending(remote));
    restored.rejectAttachmentContext();
    try testing.expect(!restored.restoredAttachmentMatches(remote));
    try testing.expectEqualDeep(snapshot, try restored.topologySnapshot());
}

test "attachment fingerprint includes full provider host ID and context" {
    const session = try createDefaultSession();
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    const tree = model.tree(0).?;
    const leaf = tree.focus;
    const remote = try remoteRef(72);
    tree.nodes[leaf].terminal = remote;
    const original = model.topologyFingerprint();
    tree.nodes[leaf].terminal = try remoteRef(73);
    try testing.expect(original != model.topologyFingerprint());
    tree.nodes[leaf].terminal = remote;
    tree.nodes[leaf].terminal.?.provider_id = @enumFromInt(123);
    try testing.expect(original != model.topologyFingerprint());
    tree.nodes[leaf].terminal = .{ .provider_id = .phux, .terminal_id = .{ .phux = try contract.RemoteResourceId.fromPhux(1, 72, "a" ** 255) } };
    const host_a = model.topologyFingerprint();
    tree.nodes[leaf].terminal.?.terminal_id.phux.host_storage[254] = 'b';
    try testing.expect(host_a != model.topologyFingerprint());
    const unknown = model.topologyFingerprint();
    // Vary the saved evidence directly: reconnect must keep old placement
    // context immutable rather than silently relabeling it.
    model.attachment_context = try topology.attachments.Context.init("/tmp/a.sock", "server-a", 71);
    try testing.expect(unknown != model.topologyFingerprint());
    const known = model.topologyFingerprint();
    model.attachment_context = try topology.attachments.Context.init("/tmp/a.sock", "server-b", 71);
    try testing.expect(known != model.topologyFingerprint());
}

test "context changes retain original live placement evidence before resaving" {
    const session = try createDefaultSession();
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    try model.setAttachmentContext("/tmp/a.sock", "server-a", 71);
    const remote = try remoteRef(72);
    try testing.expect(model.admitTab(remote));
    const snapshot = try model.topologySnapshot();
    try model.setAttachmentContext("/tmp/a.sock", "server-b", 71);
    try testing.expect(model.attachmentPending(remote));
    try testing.expect(!model.restoredAttachmentMatches(remote));
    model.normalizeTopology();
    try testing.expectEqualDeep(snapshot, try model.topologySnapshot());
    model.rejectAttachmentContext();
    try testing.expectEqualDeep(snapshot, try model.topologySnapshot());
}

test "explicit removal retires old context before a reused ID is admitted" {
    const session = try createDefaultSession();
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    const remote = try remoteRef(72);
    try model.setAttachmentContext("/tmp/a.sock", "server-a", 71);
    try testing.expect(model.admitTab(remote));
    const saved = try model.topologySnapshot();
    var restored = try app.restoreModel(testing.allocator, testing.io, .{ .v5 = saved });
    defer app.deinitModel(&restored);
    try restored.setAttachmentContext("/tmp/b.sock", "server-b", 71);
    restored.dropTab(1);
    try testing.expect(restored.restoredAttachmentContext(remote) == null);
    try testing.expect(restored.admitTab(remote));
    try testing.expect(!restored.attachmentPending(remote));
    const fresh = try restored.topologySnapshot();
    try testing.expectEqualStrings("server-b", fresh.references.entries[0].?.context.server_id.slice());
}

test "v5 reference table validates bounds duplicates and required usage" {
    var snapshot: app.TopologySnapshot = .{};
    const remote = try remoteRef(1);
    _ = try snapshot.references.append(.{ .terminal_ref = remote });
    try testing.expectError(error.InvalidTopology, snapshot.validate());
    snapshot.tab_count = 1;
    snapshot.window_count = 1;
    snapshot.windows[0] = .{ .tab_count = 1, .selection = .{ .tab = 0 } };
    snapshot.tabs[0] = topology.singleLeafTab(.terminal_1);
    snapshot.tabs[0].nodes[0].remote_ref = 0;
    try snapshot.validate();
    snapshot.tabs[0].nodes[0].remote_ref = 31;
    try testing.expectError(error.InvalidTopology, snapshot.validate());
    snapshot.tabs[0].nodes[0].remote_ref = 0;
    _ = try snapshot.references.append(.{ .terminal_ref = remote });
    try testing.expectError(error.InvalidTopology, snapshot.validate());
    snapshot.references.count = 255;
    try testing.expectError(error.InvalidTopology, snapshot.validate());
    try testing.expectError(error.AttachmentContextTooLong, topology.attachments.Context.init("e" ** 257, "s", 1));
    try testing.expectError(error.AttachmentContextTooLong, topology.attachments.Context.init("e", "s" ** 256, 1));
}

test "local v4 files migrate without attachment context or remote substitutions" {
    const bytes = "phux-cockpit-state 4\nplacement side\nwindow tab 0\ntab 0 0\nnode 0 leaf - 3\ncwd 3 /a directory\nend\n";
    var parsed: app.PersistedTopologySnapshot = undefined;
    try testing.expect(app.parseWorkspaceState(bytes, &parsed));
    const snapshot = try app.migrateTopologySnapshot(parsed);
    try testing.expectEqual(@as(u16, 5), snapshot.version);
    try testing.expectEqual(@as(u8, 0), snapshot.references.count);
    try testing.expectEqualStrings("/a directory", snapshot.cwds[3].slice());
    var restored = try app.restoreModel(testing.allocator, testing.io, parsed);
    defer app.deinitModel(&restored);
    try testing.expectEqual(@as(usize, 1), restored.provider.liveShellCount());
    try testing.expect(restored.selectedTerminalRef().?.eql(app.initialTerminalRef(3)));
}

test "maximal remote reference state fits its byte ceiling and restores no shells" {
    var snapshot: app.TopologySnapshot = .{ .window_count = 2, .tab_count = 32 };
    snapshot.windows[0] = .{ .tab_count = 16, .selection = .{ .tab = 15 } };
    snapshot.windows[1] = .{ .tab_count = 16, .selection = .{ .tab = 0 } };
    const context = try topology.attachments.Context.init("e" ** 256, "s" ** 255, std.math.maxInt(u32));
    for (0..32) |index| {
        const ref: contract.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{
            .phux = try contract.RemoteResourceId.fromPhux(1, @intCast(index), "h" ** 255),
        } };
        const slot = try snapshot.references.append(.{ .terminal_ref = ref, .context = context });
        snapshot.tabs[index] = topology.singleLeafTab(.terminal_1);
        snapshot.tabs[index].nodes[0].remote_ref = slot;
        snapshot.cwds[index].set("/" ++ "d" ** 255);
    }
    var bytes: [app.max_state_bytes]u8 = undefined;
    const encoded = try app.serializeWorkspaceState(&snapshot, &bytes);
    try testing.expect(encoded.len < app.max_state_bytes);
    var parsed: app.PersistedTopologySnapshot = undefined;
    try testing.expect(app.parseWorkspaceState(encoded, &parsed));
    try testing.expectEqualDeep(snapshot, try app.migrateTopologySnapshot(parsed));
    var restored = try app.restoreModel(testing.allocator, testing.io, parsed);
    defer app.deinitModel(&restored);
    try testing.expectEqual(@as(usize, 0), restored.provider.liveShellCount());
    var refs: [32]contract.TerminalRef = undefined;
    try testing.expectEqual(@as(usize, 32), restored.pendingRestoredRefs(&refs));
    restored.normalizeTopology();
    try testing.expectEqual(@as(usize, 32), restored.pendingRestoredRefs(&refs));
    try testing.expect(!restored.canAddPane());
    try testing.expect(!restored.admitTab(app.initialTerminalRef(0)));
    restored.dropTab(0);
    try testing.expect(restored.canAddPane());
    try testing.expect(restored.admitTab(app.initialTerminalRef(0)));
}

test "v5 parser rejects oversized malformed and unsupported reference records" {
    var parsed: app.PersistedTopologySnapshot = undefined;
    const provider_id = @intFromEnum(contract.ProviderId.phux);
    var bytes: [app.max_state_bytes]u8 = undefined;
    const records = [_][]const u8{
        "0 0 - - - 1", // valid local tagged identity, handled separately below
        "2 0 - - - 1",
        "1 0 - - - 1",
        "0 -1 - - - 1",
        "0 4294967296 - - - 1",
        "0 1 z0 - - 1",
        "0 1 0 - - 1",
        "0 1 61 - - 1",
        "1 1 ff - - 1",
        "0 1 " ++ "61" ** 256 ++ " - - 1",
        "0 1 - " ++ "61" ** 257 ++ " - 1",
        "0 1 - - " ++ "61" ** 256 ++ " 1",
    };
    for (records, 0..) |record, index| {
        const encoded = try std.fmt.bufPrint(&bytes, "phux-cockpit-state 5\nref 0 {d} {s}\nwindow tab 0\ntab 0 0\nnode 0 remote - 0\nend\n", .{ provider_id, record });
        const accepted = app.parseWorkspaceState(encoded, &parsed);
        try testing.expectEqual(index == 0, accepted);
    }
    try testing.expect(!app.parseWorkspaceState("phux-cockpit-state 4\nref 0 1 0 1 - - - 1\nend\n", &parsed));
    try testing.expect(!app.parseWorkspaceState("phux-cockpit-state 5\nref 0 1 0 1 - - - 1\nend\n", &parsed));
}

test "legacy migration turns each old terminal into a tab and an old split into a branch" {
    // v0 had no tab/pane distinction at all; v1 had a fixed two-pane
    // workspace. Both are readable ONLY through migration — `validate`
    // rejects either payload handed in as current.
    const migrated = try app.migrateTopologySnapshot(.{ .v0 = .{
        .terminal_count = 4,
        .selected_index = 3,
        .split = true,
        .split_fraction = 0.7,
    } });
    try testing.expectEqual(app.topology_snapshot_version, migrated.version);
    // Four terminals, of which two merged into one split tab: three tabs.
    try testing.expectEqual(@as(u8, 3), migrated.tab_count);
    // The split's two terminals merge into ONE tab, placed where the FIRST
    // of them stood in the old tab order (terminal 4, the selected one), and
    // that tab inherits the selection.
    try testing.expect(app.primarySnapshotSelection(&migrated).eql(.{ .tab = 2 }));
    const split_tab = migrated.tabs[2];
    try testing.expectEqual(app.Kind.branch, split_tab.nodes[split_tab.root].kind);
    try testing.expectApproxEqAbs(@as(f32, 0.7), split_tab.nodes[split_tab.root].fraction, 0.0001);

    var future = migrated;
    future.version = 99;
    try testing.expectError(error.UnsupportedTopologyVersion, app.migrateTopologySnapshot(.{ .v4 = future }));
    try testing.expectError(error.InvalidTopology, app.migrateTopologySnapshot(.{ .v0 = .{ .terminal_count = 5 } }));

    // A v1 payload with one terminal per tab migrates one-for-one.
    const from_v1 = try app.migrateTopologySnapshot(.{ .v1 = .{
        .terminal_count = 2,
        .terminal_order = .{ .terminal_1, .terminal_2, .terminal_1, .terminal_1 },
        .selection = .terminal_2,
        .attachments = .{ .terminal_1, null },
        .tab_placement = .side,
    } });
    try testing.expectEqual(@as(u8, 2), from_v1.tab_count);
    try testing.expect(app.primarySnapshotSelection(&from_v1).eql(.{ .tab = 1 }));
    try testing.expectEqual(app.TabPlacement.side, from_v1.tab_placement);
    // A duplicate in the old tab order is not migratable topology.
    try testing.expectError(error.InvalidTopology, app.migrateTopologySnapshot(.{ .v1 = .{
        .terminal_count = 2,
        .terminal_order = .{ .terminal_1, .terminal_1, .terminal_1, .terminal_1 },
    } }));
}

test "a corrupt, truncated, empty, or future state file falls back to a fresh launch" {
    var parsed: app.PersistedTopologySnapshot = undefined;

    // Nothing at all.
    try testing.expect(!app.parseWorkspaceState("", &parsed));
    try testing.expect(!app.parseWorkspaceState("\n\n\n", &parsed));

    // Somebody else's file.
    try testing.expect(!app.parseWorkspaceState("# phux cockpit config\nfont-size = 13\n", &parsed));

    // A version from the future is refused rather than read optimistically:
    // a newer build may mean something different by the same keywords.
    try testing.expect(!app.parseWorkspaceState(
        "phux-cockpit-state 99\nplacement top\nselection web\nend\n",
        &parsed,
    ));
    // ...and one from before the oldest readable schema.
    try testing.expect(!app.parseWorkspaceState(
        "phux-cockpit-state 1\nplacement top\nselection web\nend\n",
        &parsed,
    ));
    try testing.expect(!app.parseWorkspaceState("phux-cockpit-state\n", &parsed));
    try testing.expect(!app.parseWorkspaceState("phux-cockpit-state x\nend\n", &parsed));

    const whole =
        "phux-cockpit-state 3\n" ++
        "placement top\n" ++
        "selection tab 0\n" ++
        "tab 2 1\n" ++
        "node 0 leaf 2 0\n" ++
        "node 1 leaf 2 1\n" ++
        "node 2 branch - horizontal 0.5 0 1\n" ++
        "end\n";
    try testing.expect(app.parseWorkspaceState(whole, &parsed));

    // TRUNCATION at every line boundary. Each prefix is a syntactically
    // perfect file describing a SMALLER workspace, which is exactly why the
    // terminator exists: without it, a half-written save would silently
    // restore half a window.
    var cut: usize = 0;
    while (cut < whole.len) : (cut += 1) {
        if (whole[cut] != '\n') continue;
        if (cut + 1 == whole.len) continue;
        try testing.expect(!app.parseWorkspaceState(whole[0 .. cut + 1], &parsed));
    }

    // Structural corruption inside an otherwise well-formed file.
    try testing.expect(!app.parseWorkspaceState(whole ++ "tab 0 0\n", &parsed));
    try testing.expect(!app.parseWorkspaceState(
        "phux-cockpit-state 3\nnode 0 leaf - 0\nend\n",
        &parsed,
    ));
    try testing.expect(!app.parseWorkspaceState(
        "phux-cockpit-state 3\ntab 0 0\nnode 0 leaf - 0\nnode 0 leaf - 1\nend\n",
        &parsed,
    ));
    try testing.expect(!app.parseWorkspaceState(
        "phux-cockpit-state 3\ntab 0 0\nnode 0 leaf - 999\nend\n",
        &parsed,
    ));
    try testing.expect(!app.parseWorkspaceState(
        "phux-cockpit-state 3\ntab 0 0\nnode 0 branch - sideways 0.5 0 1\nend\n",
        &parsed,
    ));
    try testing.expect(!app.parseWorkspaceState(
        "phux-cockpit-state 3\ntab 0 0\nnode 0 branch - horizontal nan 0 1\nend\n",
        &parsed,
    ));
    try testing.expect(!app.parseWorkspaceState("phux-cockpit-state 3\nteleport 4\nend\n", &parsed));

    // RANDOM BYTES. Two hundred files of noise, plus noise carrying the magic
    // so the scan actually reaches the line loop. None may crash, hang, or
    // parse: the grammar has no nesting, so the work is one linear pass
    // whatever the bytes say.
    var prng = std.Random.DefaultPrng.init(0x5eed_c0de);
    const random = prng.random();
    var noise: [1024]u8 = undefined;
    for (0..200) |_| {
        const len = random.uintLessThan(usize, noise.len);
        random.bytes(noise[0..len]);
        _ = app.parseWorkspaceState(noise[0..len], &parsed);

        var seeded: [1024 + 32]u8 = undefined;
        const header = "phux-cockpit-state 3\n";
        @memcpy(seeded[0..header.len], header);
        @memcpy(seeded[header.len..][0..len], noise[0..len]);
        _ = app.parseWorkspaceState(seeded[0 .. header.len + len], &parsed);
    }
}

test "migration-invalid state is rejected without changing its source" {
    const root = ".zig-cache/phux-cockpit-invalid-migration-tests";
    const path = root ++ "/workspace.state";
    const original =
        "phux-cockpit-state 3\n" ++
        "placement top\n" ++
        "selection tab 0\n" ++
        "tab 2 1\n" ++
        "node 0 leaf 2 0\n" ++
        "node 1 leaf 2 1\n" ++
        "node 2 branch - horizontal 0.99 0 1\n" ++
        "end\n";
    var parsed: app.PersistedTopologySnapshot = undefined;
    try testing.expect(app.parseWorkspaceState(original, &parsed));
    try testing.expectError(error.InvalidTopology, app.migrateTopologySnapshot(parsed));

    const cwd = std.Io.Dir.cwd();
    cwd.deleteTree(testing.io, root) catch {};
    defer cwd.deleteTree(testing.io, root) catch {};
    try cwd.createDirPath(testing.io, root);
    try cwd.writeFile(testing.io, .{ .sub_path = path, .data = original });

    var snapshot: app.TopologySnapshot = .{};
    switch (try app.restoreWorkspace(
        testing.allocator,
        testing.io,
        path,
        &snapshot,
        1024 * 1024,
    )) {
        .rejected_existing => |preserved_path| try testing.expectEqualStrings(path, preserved_path),
        .missing => return error.TestExpectedRejectedState,
        .restored => |maybe_model| {
            var unexpected = maybe_model orelse return error.TestExpectedRejectedState;
            defer app.deinitModel(&unexpected);
            return error.TestExpectedRejectedState;
        },
    }

    var preserved: [original.len + 1]u8 = undefined;
    var file = try cwd.openFile(testing.io, path, .{});
    defer file.close(testing.io);
    const read = try file.readPositionalAll(testing.io, &preserved, 0);
    try testing.expectEqual(original.len, read);
    try testing.expectEqualStrings(original, preserved[0..read]);
}

test "a version 2 state file migrates into the current schema with no working directories" {
    // v2 is the tree schema before directories. The tree arrives whole; every
    // pane opens where a pane whose shell never reported OSC 7 opens.
    var parsed: app.PersistedTopologySnapshot = undefined;
    try testing.expect(app.parseWorkspaceState(
        "phux-cockpit-state 2\n" ++
            "placement side\n" ++
            "selection tab 0\n" ++
            "tab 2 1\n" ++
            "node 0 leaf 2 0\n" ++
            "node 1 leaf 2 1\n" ++
            "node 2 branch - vertical 0.25 0 1\n" ++
            "end\n",
        &parsed,
    ));
    try testing.expect(parsed == .v2);

    const migrated = try app.migrateTopologySnapshot(parsed);
    try testing.expectEqual(app.topology_snapshot_version, migrated.version);
    try testing.expectEqual(@as(u8, 1), migrated.tab_count);
    try testing.expectEqual(app.TabPlacement.side, migrated.tab_placement);
    try testing.expectEqual(app.Orientation.vertical, migrated.tabs[0].nodes[2].orientation);
    for (migrated.cwds) |cwd| try testing.expectEqual(@as(u16, 0), cwd.len);

    // A v2 file may not carry one: the keyword did not exist in that schema,
    // and quietly accepting it would make the version number a lie.
    try testing.expect(!app.parseWorkspaceState(
        "phux-cockpit-state 2\ntab 0 0\nnode 0 leaf - 0\ncwd 0 /tmp\nend\n",
        &parsed,
    ));
}

test "a working directory that cannot be honoured is not recorded" {
    var cwd: app.SnapshotCwd = .{};
    cwd.set("/Users/phall/work");
    try testing.expectEqualStrings("/Users/phall/work", cwd.slice());

    // Relative paths cannot be restored into (`paneArgvIn` refuses them), a
    // newline would break the line format, and a NUL would be truncated at the
    // C boundary. Each records NOTHING, which restores as "$HOME".
    cwd.set("relative/path");
    try testing.expectEqual(@as(u16, 0), cwd.len);
    cwd.set("/has\nnewline");
    try testing.expectEqual(@as(u16, 0), cwd.len);
    cwd.set("/has\x00nul");
    try testing.expectEqual(@as(u16, 0), cwd.len);
    cwd.set("/" ++ "x" ** app.max_snapshot_cwd_bytes);
    try testing.expectEqual(@as(u16, 0), cwd.len);

    // And a snapshot cannot be made to carry one behind `set`'s back.
    var snapshot: app.TopologySnapshot = .{ .tab_count = 0 };
    snapshot.cwds[0].bytes[0] = 'x';
    snapshot.cwds[0].len = 1;
    try testing.expectError(error.InvalidTopology, snapshot.validate());
}

test "normalizing topology drops panes whose terminal is gone and collapses empty tabs" {
    const session = try createDefaultSession();
    var model = app.initialModel(session);
    defer app.deinitModel(&model);

    // A tab whose only pane names a terminal nobody has is not a tab.
    _ = model.admitTab(try remoteRef(72));
    try testing.expectEqual(@as(usize, 2), model.ws().tab_count);
    model.normalizeTopology();
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
    try testing.expect(model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));

    // Web stays a CHOICE: normalization must not yank the operator back to
    // a terminal just because the web surface is up.
    model.selectWeb();
    model.normalizeTopology();
    try testing.expect(model.selectedSurface().eql(.web));
}

test "a debug build does not write the installed app's layout file" {
    // The installed app and a binary run out of `zig-out/bin` belong to the
    // same user, so they resolved the same state path — and a dev build is
    // precisely the one most likely to be running a newer schema. Whichever
    // lost that race opened a fresh window, which reads to the person using it
    // as the app having forgotten their windows.
    //
    // Asserted against the ACTUAL optimize mode rather than assuming Debug,
    // so running the suite with -Doptimize=ReleaseSafe checks the release
    // half of the switch instead of failing on a hardcoded name.
    try testing.expectEqualStrings("workspace.state", app.release_state_file_name);
    if (@import("builtin").mode == .Debug) {
        try testing.expectEqualStrings("workspace-dev.state", app.state_file_name);
        try testing.expect(!std.mem.eql(u8, app.state_file_name, app.release_state_file_name));
    } else {
        // A packaged build (package-macos.sh is ReleaseSafe) must land on the
        // real file, or an installed app would quietly stop restoring.
        try testing.expectEqualStrings(app.release_state_file_name, app.state_file_name);
    }

    // ...and that the difference survives into the RESOLVED path, so the
    // separation is a real file rather than a name nothing consumes.
    var dir_storage: [std.fs.max_path_bytes]u8 = undefined;
    var path_storage: [std.fs.max_path_bytes]u8 = undefined;
    const path = app.resolveStatePath(
        .{ .home = "/Users/someone" },
        null,
        &dir_storage,
        &path_storage,
    ) orelse return error.TestExpectedStatePath;
    try testing.expect(std.mem.endsWith(u8, path, app.state_file_name));
    if (@import("builtin").mode == .Debug) {
        try testing.expect(!std.mem.endsWith(u8, path, "/" ++ app.release_state_file_name));
    }

    // The explicit override still beats both — the escape hatch for separating
    // two builds of the same optimize mode.
    const overridden = app.resolveStatePath(
        .{ .home = "/Users/someone" },
        "/tmp/explicit.state",
        &dir_storage,
        &path_storage,
    ) orelse return error.TestExpectedStatePath;
    try testing.expectEqualStrings("/tmp/explicit.state", overridden);
}
