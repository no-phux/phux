const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const support = @import("support.zig");

const geometry = native_sdk.geometry;
const testing = std.testing;
const TerminalApp = support.TerminalApp;

const createSession = support.createSession;
const activeSlots = support.activeSlots;
const destroyModelSessions = app.deinitModel;
const startFocusedTerminal = support.startFocusedTerminal;

const JournalBuffer = struct {
    bytes: [512 * 1024]u8 = undefined,
    len: usize = 0,

    fn sink(self: *JournalBuffer) native_sdk.runtime.SessionRecorderSink {
        return .{ .context = self, .write_fn = write };
    }

    fn write(context: *anyopaque, bytes: []const u8) anyerror!void {
        const self: *JournalBuffer = @ptrCast(@alignCast(context));
        if (self.len + bytes.len > self.bytes.len) return error.NoSpaceLeft;
        @memcpy(self.bytes[self.len .. self.len + bytes.len], bytes);
        self.len += bytes.len;
    }

    fn journalBytes(self: *const JournalBuffer) []const u8 {
        return self.bytes[0..self.len];
    }
};

/// Build a `SessionRecorder` DIRECTLY into caller-owned storage.
///
/// `SessionRecorder` is ~3 MB by value and `TerminalApp` ~4.5 MB, and a
/// debug build materialises a returned aggregate as a temporary in the
/// CALLER's frame that then lives for the rest of the function. Built
/// inline, this file's recorder fixture carried ~7.9 MB of dead stack
/// through its whole body — on top of the ~4.8 MB the test frame around
/// it carried for the same reason. That was survivable until the packed
/// cell grid landed: `canvas.Builder` now embeds a 32k-cell store, and
/// the SDK's chrome installer holds two builders plus two per-view
/// command arrays on ONE frame (~4.1 MB, up ~1.3 MB), which put this
/// path over the 16 MB main-thread stack and crashed it with SIGSEGV
/// inside the SDK's frame rebuild. Constructing in a LEAF helper keeps
/// the temporary in a frame that pops immediately.
fn initSessionRecorder(slot: *native_sdk.runtime.SessionRecorder, sink: native_sdk.runtime.SessionRecorderSink) void {
    slot.* = native_sdk.runtime.SessionRecorder.init(sink);
}

/// Drive one recorded terminal session against the scriptable fake pty:
/// spawn (init_fx), a prompt, typed input (echoed by the script), and
/// the exit. Returns the recorded model and the state fingerprint.
const RecordedTerminalSession = struct {
    fingerprint: u64,
    screen: [256]u8 = undefined,
    screen_len: usize = 0,
};

fn recordTerminalSession(
    gpa: std.mem.Allocator,
    buffer: *JournalBuffer,
    store: *native_sdk.runtime.session_blobs.MemoryBlobStore,
) !RecordedTerminalSession {
    const recorder = try std.heap.page_allocator.create(native_sdk.runtime.SessionRecorder);
    defer std.heap.page_allocator.destroy(recorder);
    initSessionRecorder(recorder, buffer.sink());
    recorder.blob_sink = store.sink();
    recorder.begin(.{ .platform_name = "test", .app_name = app.app_name, .window_width = 980, .window_height = 640 });

    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;
    harness.runtime.options.session_recorder = recorder;

    const session = try createSession(80, 24);
    const app_state = try gpa.create(TerminalApp);
    defer gpa.destroy(app_state);
    support.initTerminalApp(app_state, session);
    defer app.deinitModel(&app_state.model);
    defer app_state.deinit();
    app_state.effects.executor = .fake;
    const app_iface = app_state.app();

    try harness.start(app_iface);
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = geometry.SizeF.init(980, 640),
        .scale_factor = 2,
        .frame_index = 1,
        .timestamp_ns = 1_000_000,
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    // init_fx spawned the shell against the fake pty.
    try testing.expectEqual(@as(usize, 1), app_state.effects.pendingPtyCount());

    // The scripted shell: prompt, then a typed command's echo + output.
    try app_state.effects.feedPtyOutput(1, "demo$ ");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    try testing.expectEqual(app.Phase.live, app_state.model.provider.slots[0].phase);

    // Focus the surface with a click (a real session focuses on first
    // click/key), then type: committed text routes to the app as
    // target-less text.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .pointer_down,
        .x = 200,
        .y = 200,
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .text = "ls",
    } });
    try testing.expectEqualStrings("ls", app_state.effects.ptyWrittenBytes(1));
    try app_state.effects.feedPtyOutput(1, "ls\r\nREADME.md  src\r\ndemo$ ");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    // The recording deliberately stops with the shell STILL RUNNING.
    //
    // It used to end by feeding an exit, which was harmless when an abnormal
    // end left its pane standing. It is not harmless now: an exit closes its
    // pane at any status, and closing frees the emulator immediately — so a
    // journal ending in one replays to a workspace with no terminal in it,
    // and the byte-identical screen this whole test exists to prove has
    // nothing left to read it from. Exit and close behaviour is covered by
    // the lifecycle suite; what belongs here is the replay of live state.
    recorder.finish();
    try testing.expect(!recorder.failed);

    var result: RecordedTerminalSession = .{
        .fingerprint = harness.runtime.sessionStateFingerprint(),
    };
    const screen = try session.plainText(gpa);
    defer gpa.free(screen);
    result.screen_len = @min(screen.len, result.screen.len);
    @memcpy(result.screen[0..result.screen_len], screen[0..result.screen_len]);
    return result;
}

test "the session fingerprint covers real cells, not just byte counters" {
    const gpa = testing.allocator;
    // Two sessions fed the SAME number of output bytes with different
    // contents: identical counters, different screens. The grid's
    // accessibility surface carries the viewport text, so the state
    // fingerprint (the a11y-tree hash) must differ — a VT regression
    // that garbles cells while preserving lengths can never verify.
    var fingerprints: [2]u64 = undefined;
    const outputs = [2][]const u8{ "demo$ AB", "demo$ BA" };
    for (outputs, 0..) |output, index| {
        const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
        defer harness.destroy(gpa);
        const app_state = try startFocusedTerminal(gpa, harness);
        defer gpa.destroy(app_state);
        defer destroyModelSessions(&app_state.model);
        defer app_state.deinit();
        const app_iface = app_state.app();
        try app_state.effects.feedPtyOutput(1, output);
        try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
        try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
        fingerprints[index] = harness.runtime.sessionStateFingerprint();
        try testing.expect(fingerprints[index] != 0);
    }
    try testing.expect(fingerprints[0] != fingerprints[1]);
}

test "a recorded terminal session replays byte-identical offline - no shell present" {
    const gpa = testing.allocator;
    const buffer = try std.heap.page_allocator.create(JournalBuffer);
    defer std.heap.page_allocator.destroy(buffer);
    buffer.len = 0;
    var store = native_sdk.runtime.session_blobs.MemoryBlobStore.init(gpa);
    defer store.deinit();

    const recorded = try recordTerminalSession(gpa, buffer, &store);
    try testing.expect(std.mem.indexOf(u8, recorded.screen[0..recorded.screen_len], "README.md") != null);

    // Replay into a FRESH emulator and app: the journal (events) plus
    // the blob store (output bytes) are the whole world.
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;
    const session = try createSession(80, 24);
    const app_state = try gpa.create(TerminalApp);
    defer gpa.destroy(app_state);
    support.initTerminalApp(app_state, session);
    defer app.deinitModel(&app_state.model);
    defer app_state.deinit();

    const report = try native_sdk.runtime.replaySession(&harness.runtime, app_state.app(), buffer.journalBytes(), .{
        .verify = true,
        .require_same_platform = false,
        .blobs = store.source(),
    });
    try testing.expect(report.ok());
    try testing.expect(report.checkpoints_verified > 0);
    // No process ran: the replayed spawn parked, three journaled results
    // fed (two output batches and the typed input's write-admission
    // verdict). The recording ends with the shell still live — see
    // `recordTerminalSession`.
    try testing.expectEqual(@as(u64, 3), report.effects_fed);
    try testing.expectEqual(recorded.fingerprint, harness.runtime.sessionStateFingerprint());

    // The replayed emulator rebuilt the identical screen from the
    // blob-store bytes — byte-identical, offline.
    const screen = try session.plainText(gpa);
    defer gpa.free(screen);
    try testing.expectEqualStrings(recorded.screen[0..recorded.screen_len], screen[0..recorded.screen_len]);
    // The replayed terminal is still live, which is what makes the screen
    // above readable at all.
    try testing.expectEqual(@as(usize, 1), app_state.model.provider.activeCount());
}
