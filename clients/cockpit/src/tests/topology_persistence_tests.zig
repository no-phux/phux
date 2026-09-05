const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const support = @import("support.zig");

const testing = std.testing;

const createDefaultSession = support.createDefaultSession;
const startCockpit = support.startCockpit;
const startFocusedTerminal = support.startFocusedTerminal;
const stopCockpit = support.stopCockpit;
const remoteRef = support.remoteTerminalRef;

test "versioned snapshot restores the tab trees into fresh sessions without process state" {
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, .{ .move_terminal = -1 }, &state.effects);
    // A real split inside the selected tab: the snapshot has to carry a
    // BRANCH, which the old two-pane schema could not express.
    app.update(&state.model, .split_down, &state.effects);
    const branch = state.model.ws().tabs[state.model.ws().selected_tab].root;
    state.model.ws().tabs[state.model.ws().selected_tab].setFraction(branch, 0.63);
    state.model.tab_placement = .side;
    const live_id = state.model.selectedTerminalId().?;
    const live_session = state.model.provider.terminal(live_id).?.session;
    live_session.feed("runtime state is intentionally not persisted");

    const snapshot = try state.model.topologySnapshot();
    try snapshot.validate();
    var restored = try app.restoreModel(testing.allocator, testing.io, .{ .v4 = snapshot });
    defer app.deinitModel(&restored);
    try testing.expectEqual(snapshot.version, app.topology_snapshot_version);
    try testing.expectEqual(state.model.ws().tab_count, restored.ws().tab_count);
    try testing.expectEqual(state.model.ws().selected_tab, restored.ws().selected_tab);
    try testing.expectEqual(state.model.tab_placement, restored.tab_placement);

    // The tree shape survives, fraction and focus included.
    for (0..state.model.ws().tab_count) |index| {
        const source = state.model.treeConst(index).?;
        const target = restored.treeConst(index).?;
        try testing.expectEqual(source.paneCount(), target.paneCount());
        try testing.expectEqual(source.focus, target.focus);
        try testing.expectEqual(source.isSplit(), target.isSplit());
    }
    const restored_branch = restored.ws().tabs[restored.ws().selected_tab].root;
    try testing.expectApproxEqAbs(
        @as(f32, 0.63),
        restored.ws().tabs[restored.ws().selected_tab].node(restored_branch).fraction,
        0.0001,
    );
    try testing.expectEqual(
        app.Orientation.vertical,
        restored.ws().tabs[restored.ws().selected_tab].node(restored_branch).orientation,
    );

    const fresh = restored.provider.terminal(live_id) orelse return error.TestExpectedTerminal;
    try testing.expect(fresh.session != live_session);
    try testing.expectEqual(@as(u64, 0), fresh.output_bytes);
    try testing.expectEqual(app.Phase.starting, fresh.phase);
    try testing.expectEqual(@as(u64, 0), fresh.session_generation);
    const text = try fresh.session.plainText(testing.allocator);
    defer testing.allocator.free(text);
    try testing.expect(std.mem.indexOf(u8, text, "runtime state") == null);
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

test "canonical snapshots reject topology rewrites and round trip exactly" {
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, .split_right, &state.effects);
    const branch = state.model.ws().tabs[state.model.ws().selected_tab].root;
    state.model.ws().tabs[state.model.ws().selected_tab].setFraction(branch, 0.63);

    const snapshot = try state.model.topologySnapshot();
    var restored = try app.restoreModel(testing.allocator, testing.io, .{ .v4 = snapshot });
    defer app.deinitModel(&restored);
    try testing.expectEqualDeep(snapshot, try restored.topologySnapshot());
    try testing.expect(!app.process_restoration_supported);

    const selected: usize = switch (app.primarySnapshotSelection(&snapshot)) {
        .tab => |index| index,
        .web => return error.TestExpectedTabSelection,
    };

    // A tab whose focus is a BRANCH is not a workspace: focus is a pane.
    var invalid = snapshot;
    invalid.tabs[selected].focus = invalid.tabs[selected].root;
    try testing.expectError(error.InvalidTopology, invalid.validate());

    // Two panes may never name the SAME terminal, in this tab or another.
    invalid = snapshot;
    const root_node = invalid.tabs[selected].nodes[invalid.tabs[selected].root];
    invalid.tabs[selected].nodes[root_node.second].terminal = invalid.tabs[selected].nodes[root_node.first].terminal;
    try testing.expectError(error.InvalidTopology, invalid.validate());

    // A branch fraction outside the layout bounds is refused.
    invalid = snapshot;
    invalid.tabs[selected].nodes[invalid.tabs[selected].root].fraction = 0.99;
    try testing.expectError(error.InvalidTopology, invalid.validate());

    // A selection naming a tab that is not there is refused.
    invalid = snapshot;
    invalid.windows[0].selection = .{ .tab = invalid.tab_count };
    try testing.expectError(error.InvalidTopology, invalid.validate());

    // A terminal id past the registry ceiling is refused.
    invalid = snapshot;
    invalid.tabs[0].nodes[invalid.tabs[0].focus].terminal = @enumFromInt(std.math.maxInt(u64) - 1);
    try testing.expectError(error.InvalidTopology, invalid.validate());
}

/// A fired one-shot debounce, as the fx timer would deliver it.
fn debounceFired() app.Msg {
    return .{ .persist_topology = .{
        .key = app.topology_persist_timer_key,
        .timestamp_ns = 0,
        .outcome = .fired,
    } };
}

fn writeCompleted(outcome: native_sdk.EffectFileOutcome) app.Msg {
    return .{ .topology_persisted = .{
        .key = app.topology_state_file_key,
        .op = .write,
        .outcome = outcome,
    } };
}

fn fireTopologyTimer(harness: anytype, state: anytype) !void {
    try state.effects.fireTimer(app.topology_persist_timer_key);
    try harness.runtime.dispatchPlatformEvent(state.app(), .wake);
}

fn completeTopologyWrite(harness: anytype, state: anytype, outcome: native_sdk.EffectFileOutcome) !void {
    try state.effects.feedFileResult(app.topology_state_file_key, outcome, "");
    try harness.runtime.dispatchPlatformEvent(state.app(), .wake);
}

const state_path = "/tmp/phux-cockpit-tests/workspace.state";

test "the state file round trips a split workspace, its divider, and its working directories" {
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, .split_right, &state.effects);
    const branch = state.model.ws().tabs[state.model.ws().selected_tab].root;
    state.model.ws().tabs[state.model.ws().selected_tab].setFraction(branch, 0.63);
    state.model.tab_placement = .side;

    // The directory is the shell's own OSC 7 report, which is the only place
    // a cwd ever comes from.
    const focused = state.model.selectedTerminalId().?;
    state.model.provider.terminal(focused).?.session.feed("\x1b]7;file://host/Users/phall/my%20dir\x1b\\");

    const snapshot = try state.model.topologySnapshot();
    try testing.expectEqualStrings("/Users/phall/my dir", snapshot.cwdFor(app.localId(focused).?));

    var bytes: [app.max_state_bytes]u8 = undefined;
    const encoded = try app.serializeWorkspaceState(&snapshot, &bytes);
    var parsed: app.PersistedTopologySnapshot = undefined;
    try testing.expect(app.parseWorkspaceState(encoded, &parsed));
    // Byte-for-byte: the fraction survives because `{d}` prints the shortest
    // decimal that reads back as the same f32, so a divider does not creep by
    // a ulp per launch.
    try testing.expectEqualDeep(snapshot, try app.migrateTopologySnapshot(parsed));

    // And the file is text a person can read, not an opaque blob.
    try testing.expect(std.mem.startsWith(u8, encoded, "phux-cockpit-state 4\n"));
    try testing.expect(std.mem.indexOf(u8, encoded, "placement side\n") != null);
    try testing.expect(std.mem.indexOf(u8, encoded, "\ncwd 2 /Users/phall/my dir\n") != null);
    try testing.expect(std.mem.endsWith(u8, encoded, "\nend\n"));
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

test "missing state saves and valid state restores then remains writable" {
    const root = ".zig-cache/phux-cockpit-state-provenance-tests";
    const path = root ++ "/workspace.state";
    const cwd = std.Io.Dir.cwd();
    cwd.deleteTree(testing.io, root) catch {};
    defer cwd.deleteTree(testing.io, root) catch {};

    var snapshot: app.TopologySnapshot = .{};
    switch (try app.restoreWorkspace(
        testing.allocator,
        testing.io,
        path,
        &snapshot,
        1024 * 1024,
    )) {
        .missing => {},
        .rejected_existing => return error.TestExpectedMissingState,
        .restored => |maybe_model| {
            var unexpected = maybe_model orelse return error.TestExpectedMissingState;
            defer app.deinitModel(&unexpected);
            return error.TestExpectedMissingState;
        },
    }

    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    try testing.expect(!app.chromeRevealed(&state.model));
    state.model.state.setPath(path);
    app.update(&state.model, .split_right, &state.effects);
    app.update(&state.model, .shutdown, &state.effects);

    // A valid file must not turn an allocation failure into "rejected" and a
    // fresh fallback. Startup sees the resource failure.
    try testing.expectError(error.OutOfMemory, app.restoreWorkspace(
        std.testing.failing_allocator,
        testing.io,
        path,
        &snapshot,
        1024 * 1024,
    ));

    var restored_snapshot: app.TopologySnapshot = .{};
    var restored = switch (try app.restoreWorkspace(
        testing.allocator,
        testing.io,
        path,
        &restored_snapshot,
        1024 * 1024,
    )) {
        .restored => |maybe_model| maybe_model orelse return error.TestExpectedRestoredState,
        .missing => return error.TestExpectedRestoredState,
        .rejected_existing => return error.TestExpectedRestoredState,
    };
    defer app.deinitModel(&restored);
    try testing.expectEqualDeep(try state.model.topologySnapshot(), restored_snapshot);
    try testing.expect(!app.chromeRevealed(&restored));

    // The restored provenance remains writable. Change it, synchronously save
    // it, and inspect the actual file rather than only the model.
    restored.state.setPath(path);
    restored.tab_placement = .side;
    restored.writeWorkspaceState(testing.io);
    var bytes: [app.max_state_bytes]u8 = undefined;
    var file = try cwd.openFile(testing.io, path, .{});
    defer file.close(testing.io);
    const read = try file.readPositionalAll(testing.io, &bytes, 0);
    var persisted: app.PersistedTopologySnapshot = undefined;
    try testing.expect(app.parseWorkspaceState(bytes[0..read], &persisted));
    try testing.expectEqual(app.TabPlacement.side, (try app.migrateTopologySnapshot(persisted)).tab_placement);
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

test "a topology change arms one debounced write and ordinary input arms none" {
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    state.model.state.setPath(state_path);
    try testing.expect(state.model.state.enabled());

    // Input that reshapes nothing writes nothing. This is the whole reason
    // the trigger is a shape hash and not a call in each arm: a terminal
    // takes thousands of these a minute.
    app.update(&state.model, .unhover_tab, &state.effects);
    app.update(&state.model, .{ .hover_tab = 0 }, &state.effects);
    try testing.expectError(error.EffectNotFound, state.effects.fireTimer(app.topology_persist_timer_key));

    // Every shape-changing act arms exactly one pending expiry, and a BURST
    // of them still arms one: re-arming an active timer key replaces it.
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, .split_right, &state.effects);
    app.update(&state.model, .{ .move_terminal = -1 }, &state.effects);
    try state.effects.fireTimer(app.topology_persist_timer_key);
    try testing.expectError(error.EffectNotFound, state.effects.fireTimer(app.topology_persist_timer_key));

    // A divider drag reshapes the tree on every frame; the debounce is what
    // keeps that from being a write per frame.
    const branch = state.model.ws().tabs[state.model.ws().selected_tab].root;
    var step: f32 = 0.30;
    while (step < 0.60) : (step += 0.01) {
        app.update(&state.model, .{ .split_resized = .{ .node = branch, .value = step } }, &state.effects);
    }
    try state.effects.fireTimer(app.topology_persist_timer_key);
    try testing.expectError(error.EffectNotFound, state.effects.fireTimer(app.topology_persist_timer_key));
}

test "the debounced write carries the live layout and a second one waits for it" {
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    state.model.state.setPath(state_path);
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, .split_down, &state.effects);

    app.update(&state.model, debounceFired(), &state.effects);
    try testing.expectEqual(@as(usize, 1), state.effects.pendingFileCount());
    const request = state.effects.pendingFileAt(0) orelse return error.TestExpectedFileRequest;
    try testing.expectEqual(app.topology_state_file_key, request.key);
    try testing.expectEqual(native_sdk.EffectFileOp.write, request.op);
    try testing.expectEqualStrings(state_path, request.path);

    var parsed: app.PersistedTopologySnapshot = undefined;
    try testing.expect(app.parseWorkspaceState(request.bytes, &parsed));
    const written = try app.migrateTopologySnapshot(parsed);
    try testing.expectEqualDeep(try state.model.topologySnapshot(), written);

    // The SDK rejects a second write on a live key, and a rejection would
    // report a failure for a save that never started. So a change made while
    // a write is out waits for it instead of racing it.
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, debounceFired(), &state.effects);
    try testing.expectEqual(@as(usize, 1), state.effects.pendingFileCount());

    // The terminal result is the only thing that releases the key — and it
    // re-arms, because the shape moved while the write was out.
    app.update(&state.model, writeCompleted(.ok), &state.effects);
    try testing.expect(!state.model.state.inflight);
    try state.effects.fireTimer(app.topology_persist_timer_key);

    // With nothing owed, an expiry writes nothing at all.
    state.model.state.pending = false;
    try state.effects.feedFileResult(app.topology_state_file_key, .ok, "");
    app.update(&state.model, debounceFired(), &state.effects);
    try testing.expectEqual(@as(usize, 0), state.effects.pendingFileCount());
}

test "the shutdown flush writes through a symlinked parent directory" {
    // The layout has to survive a state path whose PARENT is a symlink to a
    // directory, because that is the ordinary shape of the place people put
    // one: `/tmp` is a symlink to `/private/tmp` on macOS, and a platform
    // state directory can be reached through one too.
    //
    // The fixture builds its own symlink rather than leaning on `/tmp` being
    // one, so the hazard is present on every platform this ever runs on
    // instead of only where the OS happens to supply it.
    const root = ".zig-cache/phux-cockpit-state-symlink-tests";
    const real_dir = root ++ "/real";
    const link_dir = root ++ "/link";
    const path = link_dir ++ "/workspace.state";
    const cwd = std.Io.Dir.cwd();
    cwd.deleteTree(testing.io, root) catch {};
    defer cwd.deleteTree(testing.io, root) catch {};
    try cwd.createDirPath(testing.io, real_dir);
    try cwd.symLink(testing.io, "real", link_dir, .{});

    // The HAZARD is asserted, not assumed. `createDirPath` stats each existing
    // component without following symlinks, so a symlink to a directory comes
    // back `.sym_link`, not `.directory`, and the call fails — while a write
    // straight through the same link succeeds. A fixture whose symlink
    // `createDirPath` happily accepted would pass on either ordering and would
    // prove nothing at all.
    //
    // `NotDir` rather than a looser "it failed somehow" because the error is
    // the evidence that the SYMLINK is what stopped it. It comes from
    // `Threaded.dirCreateDirPath`, which answers `PathAlreadyExists` by
    // checking `filePathKind` with `SYMLINK_NOFOLLOW` and rejects anything
    // that is not a directory. The same call against a plain existing
    // directory succeeds, and `createDirPath(io, "/tmp")` fails identically on
    // macOS -- both measured before this test was written.
    try testing.expectError(error.NotDir, cwd.createDirPath(testing.io, link_dir));

    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    state.model.state.setPath(path);
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, .split_right, &state.effects);

    // ABSENT: nothing is on disk yet, so the read below cannot be satisfied by
    // a leftover from an earlier run or by the fixture itself.
    try testing.expectError(error.FileNotFound, cwd.openFile(testing.io, path, .{}));

    app.update(&state.model, .shutdown, &state.effects);

    // PRESENT, and byte-for-byte the live layout: a save that reached the disk
    // but dropped the split would satisfy "the file exists" and nothing more.
    var bytes: [app.max_state_bytes]u8 = undefined;
    var file = try cwd.openFile(testing.io, path, .{});
    defer file.close(testing.io);
    const read = try file.readPositionalAll(testing.io, &bytes, 0);
    var parsed: app.PersistedTopologySnapshot = undefined;
    try testing.expect(app.parseWorkspaceState(bytes[0..read], &parsed));
    try testing.expectEqualDeep(try state.model.topologySnapshot(), try app.migrateTopologySnapshot(parsed));
}

test "the shutdown message puts the layout on disk without waiting for the debounce" {
    // The one claim that cannot be made without real IO: quitting inside the
    // debounce window still saves. Nothing else drains an effect queue after
    // shutdown, so the write has to be synchronous, and "synchronous" is only
    // provable against a file.
    const root = ".zig-cache/phux-cockpit-state-tests";
    const path = root ++ "/shutdown.state";
    const cwd = std.Io.Dir.cwd();
    cwd.deleteTree(testing.io, root) catch {};
    defer cwd.deleteTree(testing.io, root) catch {};

    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    state.model.state.setPath(path);
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, .split_right, &state.effects);
    // The debounce has not expired, so no effect has written anything.
    try testing.expectEqual(@as(usize, 0), state.effects.pendingFileCount());

    app.update(&state.model, .shutdown, &state.effects);

    var bytes: [app.max_state_bytes]u8 = undefined;
    var file = try cwd.openFile(testing.io, path, .{});
    defer file.close(testing.io);
    const read = try file.readPositionalAll(testing.io, &bytes, 0);
    var parsed: app.PersistedTopologySnapshot = undefined;
    try testing.expect(app.parseWorkspaceState(bytes[0..read], &parsed));
    try testing.expectEqualDeep(try state.model.topologySnapshot(), try app.migrateTopologySnapshot(parsed));
}

test "a model with no state path never touches the disk" {
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    // No composition root handed this model a path — every fixture and every
    // test is in this state, and none of them may write files.
    try testing.expect(!state.model.state.enabled());
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, .split_right, &state.effects);
    try testing.expectError(error.EffectNotFound, state.effects.fireTimer(app.topology_persist_timer_key));
    app.update(&state.model, debounceFired(), &state.effects);
    try testing.expectEqual(@as(usize, 0), state.effects.pendingFileCount());
    // Nor does shutdown: there is nowhere to write.
    app.update(&state.model, .shutdown, &state.effects);
}

test "restored panes reopen in their saved working directory" {
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    app.update(&state.model, .split_right, &state.effects);
    const focused = state.model.selectedTerminalId().?;
    state.model.provider.terminal(focused).?.session.feed("\x1b]7;file:///Users/phall/project\x1b\\");

    const snapshot = try state.model.topologySnapshot();
    var restored = try app.restoreModel(testing.allocator, testing.io, .{ .v4 = snapshot });
    defer app.deinitModel(&restored);
    app.applyRestoredWorkingDirectories(&restored, &snapshot);

    // The pane that reported a directory carries the generated `cd` argv; the
    // one that never did keeps the plain login shell, exactly as a first
    // terminal does.
    const with_cwd = restored.provider.terminal(focused) orelse return error.TestExpectedTerminal;
    try testing.expect(std.mem.indexOf(u8, with_cwd.argv[with_cwd.argv.len - 1], "/Users/phall/project") != null);

    var refs: [app.max_panes_per_tab]app.TerminalRef = undefined;
    const count = restored.treeConst(restored.ws().selected_tab).?.terminals(&refs);
    try testing.expectEqual(@as(usize, 2), count);
    for (refs[0..count]) |id| {
        if (id.eql(focused)) continue;
        const bare = restored.provider.terminal(id).?;
        try testing.expectEqualSlices(u8, app.paneArgv(0)[0], bare.argv[0]);
        try testing.expect(std.mem.indexOf(u8, bare.argv[bare.argv.len - 1], "/Users/phall/project") == null);
    }

    // Restoring recreates SHELLS, never processes: the snapshot carries no
    // pid and claims none.
    try testing.expect(!app.process_restoration_supported);
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
