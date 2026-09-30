//! Rename Session: the engine half (docs/REMOTE_HOSTS.md, "Renaming a
//! session").
//!
//! One bounded request, `cockpit.session`, carries the action and its answer:
//!
//!   request  version=1, kind:u8, name_len:u8, name UTF-8
//!            kind 1 describe the session a rename would name, 2 rename it
//!            to `name`, 3 the last rename's outcome
//!   reply    version=1, phase:u8, name_len:u8, name, host_len:u8, host,
//!            reason_len:u8, reason
//!            phase 0 ready, 1 pending, 2 renamed, 3 refused, 4 unavailable
//!
//! The session is the one on screen: the session whose tab holds the focused
//! pane, on the coordinator that minted that pane's ref, and on no other.
//! With no Phux pane focused it is the active coordinator's attached session.
//! A pane whose coordinator is no longer held names nothing, so the rename is
//! refused rather than sent anywhere else. The write is
//! `phux.session.name/v1` on that coordinator's own connection; the server's
//! METADATA_CHANGED renames the session in that coordinator's list, which the
//! header and the switcher read.
//!
//! Kinds 6 and 7 name the session of one switcher row instead:
//!
//!   request  version=1, kind:u8, name_len:u8, name UTF-8, target_len:u8,
//!            target (the row's captured catalog target, catalog_targets.zig)
//!
//! The target carries the coordinator that listed the row and that
//! coordinator's context, so it resolves only against that coordinator as it
//! was when the row was captured (`rowTarget`); a coordinator no longer held,
//! retargeted or reconnected since, or no longer listing that session, names
//! nothing and nothing is sent. A listing peer's row is renamed on that peer's listing connection:
//! the write is metadata, so the peer still never attaches.
//!
//! Kinds 8/9/10/11 describe, rename, poll and retire a captured tab editor.
//! They carry the process-local tab target after the name. Description captures
//! the exact provider attachment/epoch, session and shared window; submission
//! uses its existing workspace mutation queue. Only a confirmed workspace
//! snapshot changes the name; retiring the editor never rolls back a write.

const std = @import("std");
const support = @import("../phux_support.zig");
const navigation = @import("ts_navigation.zig");
const projection = @import("workspace_projection.zig");

pub const request_name = "cockpit.session";
pub const version: u8 = 1;
/// Per-field display bound, as for `cockpit.remote`.
pub const max_text_bytes: usize = 240;
pub const max_bytes: usize = 5 + 3 * max_text_bytes;

/// 4 and 5 serve the Empty session state (empty_session.zig): New Tab in
/// the empty session a window shows, and dismissing a picked one. 6 and 7
/// are 1 and 2 for the session a switcher row names (below).
pub const Kind = enum(u8) { describe = 1, rename = 2, status = 3, new_tab = 4, dismiss = 5, describe_row = 6, rename_row = 7, describe_tab = 8, rename_tab = 9, tab_status = 10, tab_cancel = 11 };
/// `unavailable`: nothing on screen can be renamed. `refused`: this rename
/// changed nothing; the reason says why.
pub const Phase = enum(u8) { ready = 0, pending = 1, renamed = 2, refused = 3, unavailable = 4 };
pub const Error = error{ InvalidRequest, BufferTooSmall };

/// The rename sent to one coordinator, on one of its connections. `context`
/// is that provider's own lifetime (`context_id`): ids move with a retarget
/// and epochs and request ids are per-connection counters, so only the
/// context tells the provider it was sent to from one that took its id.
pub const Flight = struct { coordinator: support.ProviderId, context: u64, epoch: u64, request_id: u32 };

/// One rename slot per coordinator (each has its own connection); a pending
/// rename refuses only a second rename on the same coordinator. `last` is the
/// coordinator whose outcome `status` reports.
pub const Flights = struct {
    slots: std.ArrayList(?Flight) = .empty,
    last: ?support.ProviderId = null,

    pub fn of(self: *const Flights, coordinator: support.ProviderId) ?Flight {
        for (self.slots.items) |slot| if (slot) |flight| if (flight.coordinator == coordinator) return flight;
        return null;
    }

    /// The slot a rename on `coordinator` goes into: that coordinator's own,
    /// else a free one, else one whose rename can no longer settle
    /// (`settled(engine, flight)`). Grow only when every existing slot is
    /// occupied. Allocation refusal is reported before anything is sent.
    pub fn slotFor(self: *Flights, coordinator: support.ProviderId, engine: anytype, comptime settled: anytype) ?usize {
        for (self.slots.items, 0..) |slot, index| if (slot) |flight| if (flight.coordinator == coordinator) return index;
        for (self.slots.items, 0..) |slot, index| if (slot == null) return index;
        for (self.slots.items, 0..) |slot, index| if (settled(engine, slot.?)) return index;
        self.slots.append(std.heap.page_allocator, null) catch return null;
        return self.slots.items.len - 1;
    }

    pub fn put(self: *Flights, index: usize, flight: Flight) void {
        self.slots.items[index] = flight;
        self.last = flight.coordinator;
    }

    pub fn deinit(self: *Flights) void {
        self.slots.deinit(std.heap.page_allocator);
    }
};

pub const Request = struct { kind: Kind, name: []const u8 = "", target: []const u8 = "" };

pub fn decode(bytes: []const u8) Error!Request {
    if (bytes.len < 3 or bytes[0] != version) return error.InvalidRequest;
    const kind = std.enums.fromInt(Kind, bytes[1]) orelse return error.InvalidRequest;
    const name_end = 3 + @as(usize, bytes[2]);
    if (name_end > bytes.len) return error.InvalidRequest;
    const name = bytes[3..name_end];
    const target = try decodeTarget(kind, bytes, name_end);
    switch (kind) {
        .rename, .rename_row, .rename_tab => if (name.len == 0 or !std.unicode.utf8ValidateSlice(name)) return error.InvalidRequest,
        .describe, .status, .new_tab, .dismiss, .describe_row, .describe_tab, .tab_status, .tab_cancel => if (name.len != 0) return error.InvalidRequest,
    }
    return .{ .kind = kind, .name = name, .target = target };
}

/// A row kind's captured target follows its name; every other kind ends at
/// its name.
fn decodeTarget(kind: Kind, bytes: []const u8, at: usize) Error![]const u8 {
    if (kind != .describe_row and kind != .rename_row and kind != .describe_tab and kind != .rename_tab and kind != .tab_status and kind != .tab_cancel) {
        return if (at == bytes.len) "" else error.InvalidRequest;
    }
    if (at >= bytes.len) return error.InvalidRequest;
    const target = bytes[at + 1 ..];
    if (target.len != bytes[at] or target.len == 0) return error.InvalidRequest;
    return target;
}

pub const Reply = struct { phase: Phase, name: []const u8 = "", host: []const u8 = "", reason: []const u8 = "" };

pub fn encode(reply: Reply, out: []u8) Error![]const u8 {
    var name_buffer: [max_text_bytes]u8 = undefined;
    var host_buffer: [max_text_bytes]u8 = undefined;
    var reason_buffer: [max_text_bytes]u8 = undefined;
    const fields = [_][]const u8{
        navigation.displayText(reply.name, &name_buffer),
        navigation.displayText(reply.host, &host_buffer),
        navigation.displayText(reply.reason, &reason_buffer),
    };
    var len: usize = 2;
    for (fields) |field| len += 1 + field.len;
    if (out.len < len) return error.BufferTooSmall;
    out[0] = version;
    out[1] = @intFromEnum(reply.phase);
    var at: usize = 2;
    for (fields) |field| {
        out[at] = @intCast(field.len);
        @memcpy(out[at + 1 ..][0..field.len], field);
        at += 1 + field.len;
    }
    return out[0..len];
}

/// Storage a reply's slices borrow until it is encoded.
const Scratch = struct {
    reason: [max_text_bytes]u8 = undefined,
};

/// Apply one request on the owning thread and encode the answer.
pub fn handle(engine: anytype, fx: anytype, payload: []const u8, out: []u8) Error![]const u8 {
    const request = try decode(payload);
    var scratch: Scratch = .{};
    const reply = switch (request.kind) {
        .describe => describe(engine),
        .rename => rename(engine, request.name, &scratch),
        .describe_row => if (rowTarget(engine, request.target)) |target| describeTarget(engine, target) else row_gone,
        .rename_row => if (rowTarget(engine, request.target)) |target| renameTarget(engine, target, request.name, &scratch) else row_gone,
        .describe_tab => describeTab(engine, request.target),
        .rename_tab => renameTab(engine, request.target, request.name),
        .tab_status => tabStatus(engine, request.target),
        .tab_cancel => blk: {
            if (engine.tab_rename.matches(request.target)) retireTabRename(engine);
            break :blk Reply{ .phase = .ready };
        },
        .status => status(engine, &scratch),
        .new_tab => newTab(engine, fx),
        .dismiss => blk: {
            empty_session.dismiss(engine.model);
            break :blk Reply{ .phase = .ready };
        },
    };
    return encode(reply, out);
}

/// New Tab in the Empty session state: pending once a tab is on its way to
/// that session's own coordinator, else refused with the reason.
fn newTab(engine: anytype, fx: anytype) Reply {
    if (comptime !support.phux_enabled) return nothing_on_screen;
    return switch (empty_session.newTab(engine, fx)) {
        .opened => |view| .{ .phase = .pending, .name = view.name, .host = view.host },
        .refused => |reason| .{ .phase = .refused, .reason = reason },
    };
}

const empty_session = @import("empty_session.zig");

const nothing_on_screen: Reply = .{ .phase = .unavailable, .reason = "No Phux session is on screen to rename." };

const row_gone: Reply = .{ .phase = .unavailable, .reason = "That session is no longer listed there. Refresh the switcher and try again." };

/// A session to rename and the coordinator that owns it.
pub const Target = struct { provider: *support.PhuxProvider, session: u32, name: []const u8 };

/// The session a switcher row names, resolved by id against the coordinator
/// that listed it, as captured. Null (nothing sent) when that coordinator is
/// gone, moved on, or no longer lists it.
pub fn rowTarget(engine: anytype, bytes: []const u8) ?Target {
    if (comptime !support.phux_enabled) return null;
    const model = engine.model;
    const captured = navigation.targets.decode(bytes) orelse return null;
    const entry = captured.resolve(model) orelse return null;
    var owner: *support.PhuxProvider = undefined;
    var session: u32 = 0;
    switch (entry) {
        .session => |id| {
            owner = model.phux() orelse return null;
            session = id;
        },
        .peer_session => |row| {
            owner = model.phuxPeerAt(model.peerSlot(row.coordinator) orelse return null) orelse return null;
            session = row.id;
        },
        .placed_terminal, .available_terminal, .peer_unavailable => return null,
    }
    if (owner.providerId() != captured.provider_id or session == 0) return null;
    const state = owner.state();
    if (state != .negotiated and state != .attached) return null;
    for (owner.sessionCatalog()) |listed| {
        if (listed.id == session and listed.name.len != 0) return .{ .provider = owner, .session = session, .name = listed.name };
    }
    return null;
}

/// The session a rename would name, and whose host it is on.
pub fn describe(engine: anytype) Reply {
    const target = engine.renameTarget() orelse return nothing_on_screen;
    return describeTarget(engine, target);
}

fn describeTarget(engine: anytype, target: anytype) Reply {
    return .{ .phase = .ready, .name = target.name, .host = hostLabel(engine.model, target.provider) };
}

fn rename(engine: anytype, name: []const u8, scratch: *Scratch) Reply {
    if (comptime !support.phux_enabled) return nothing_on_screen;
    const target = engine.renameTarget() orelse return nothing_on_screen;
    return renameTarget(engine, target, name, scratch);
}

/// Rename `target`'s session on `target.provider`'s connection alone.
fn renameTarget(engine: anytype, target: anytype, name: []const u8, scratch: *Scratch) Reply {
    if (comptime !support.phux_enabled) return nothing_on_screen;
    const host = hostLabel(engine.model, target.provider);
    var reply: Reply = .{ .phase = .refused, .name = target.name, .host = host };
    // One rename at a time per coordinator; another coordinator's pending
    // rename is on another connection and does not refuse this one.
    const coordinator = target.provider.providerId();
    if (engine.rename_flights.of(coordinator)) |flight| if (flightPending(engine, flight)) {
        reply.reason = "A rename is already in progress.";
        return reply;
    };
    const slot = engine.rename_flights.slotFor(coordinator, engine, flightSettled) orelse {
        reply.reason = "A rename is already in progress.";
        return reply;
    };
    for (name) |byte| if (byte < 0x20 or byte == 0x7f) {
        reply.reason = "A session name cannot contain control characters.";
        return reply;
    };
    // Judged first against the list Cockpit shows for that coordinator; the
    // client judges it again against its own, and the server has the last word.
    for (target.provider.sessionCatalog()) |entry| {
        if (entry.id == target.session or !std.mem.eql(u8, entry.name, name)) continue;
        reply.reason = std.fmt.bufPrint(&scratch.reason, "\"{s}\" already exists on {s}.", .{ name, host }) catch "That name already exists.";
        return reply;
    }
    const request_id = target.provider.requestRename(target.name, name) catch {
        reply.reason = std.fmt.bufPrint(&scratch.reason, "Could not send the rename to {s}.", .{host}) catch "Could not send the rename.";
        return reply;
    };
    engine.rename_flights.put(slot, .{ .coordinator = coordinator, .context = target.provider.context_id, .epoch = target.provider.connectionEpoch(), .request_id = request_id });
    return outcome(engine, target.provider, request_id, scratch);
}

/// Whether the flight's coordinator still holds it pending on the same
/// connection. A flight whose connection moved on can never settle.
fn flightPending(engine: anytype, flight: Flight) bool {
    const owner = engine.model.phuxFor(flight.coordinator) orelse return false;
    if (owner.context_id != flight.context or owner.connectionEpoch() != flight.epoch) return false;
    const info = owner.renameInfo();
    return info.request_id == flight.request_id and info.status == .pending;
}

fn flightSettled(engine: anytype, flight: Flight) bool {
    return !flightPending(engine, flight);
}

/// The last rename's outcome, read from the coordinator it was sent to.
fn status(engine: anytype, scratch: *Scratch) Reply {
    const coordinator = engine.rename_flights.last orelse return describe(engine);
    const flight = engine.rename_flights.of(coordinator) orelse return describe(engine);
    const unknown: Reply = .{ .phase = .refused, .reason = "The connection ended before the rename was confirmed." };
    const owner = engine.model.phuxFor(flight.coordinator) orelse return unknown;
    if (owner.context_id != flight.context or owner.connectionEpoch() != flight.epoch) return unknown;
    return outcome(engine, owner, flight.request_id, scratch);
}

fn outcome(engine: anytype, owner: anytype, request_id: u32, scratch: *Scratch) Reply {
    const info = owner.renameInfo();
    var reply: Reply = .{ .phase = .refused, .host = hostLabel(engine.model, owner), .name = sessionName(owner, info.session_id) };
    if (info.request_id != request_id) {
        reply.reason = "The rename's outcome is unknown.";
        return reply;
    }
    switch (info.status) {
        .pending => reply.phase = .pending,
        .renamed => reply.phase = .renamed,
        .none => reply.reason = "The rename's outcome is unknown.",
        else => {
            // Borrowed from the client: copied before anything else runs.
            const message = if (info.message.len != 0) info.message else "the server refused the rename";
            const length = @min(message.len, scratch.reason.len);
            @memcpy(scratch.reason[0..length], message[0..length]);
            reply.reason = scratch.reason[0..length];
        },
    }
    return reply;
}

fn sessionName(owner: anytype, id: u32) []const u8 {
    for (owner.sessionCatalog()) |entry| if (entry.id == id) return entry.name;
    return "";
}

/// The host a coordinator's sessions are listed under: this Mac, or the
/// registered host's label, as the switcher's groups name them.
fn hostLabel(model: anytype, owner: anytype) []const u8 {
    if (model.phuxConst()) |active| if (active.providerId() == owner.providerId()) return active.remoteLabel() orelse "This Mac";
    return projection.peerHostLabel(model, owner.providerId());
}

const tab_commands = @import("tab_commands.zig");
const shared_mutations = @import("../shared_mutations.zig");

/// The dialog owns a captured attachment, session and shared window, not the
/// current selection. Closing the dialog retires only its waiter, never the SET.
pub const TabRename = struct {
    target: ?tab_commands.Target = null,
    attachment: u64 = 0,
    epoch: u64 = 0,
    session: u32 = 0,
    window: [16]u8 = @splat(0),
    ticket: ?u64 = null,
    phase: Phase = .unavailable,
    name: [max_text_bytes]u8 = undefined,
    name_len: usize = 0,

    fn matches(self: *const TabRename, bytes: []const u8) bool {
        const target = self.target orelse return false;
        const received = tab_commands.decodeCaptured(bytes) orelse return false;
        return std.meta.eql(target, received);
    }
};

const tab_unavailable: Reply = .{ .phase = .unavailable, .reason = "This tab has no connected, durable workspace to rename." };
const tab_changed: Reply = .{ .phase = .refused, .reason = "That tab or its connection changed. Close Rename and try again." };
const tab_unknown: Reply = .{ .phase = .refused, .reason = "The rename could not be confirmed. Refresh the workspace before trying again." };

fn tabRenameQueue(engine: anytype) ?*shared_mutations.Coordinator {
    if (comptime !support.phux_enabled) return null;
    if (engine.model.phux()) |remote| {
        if (remote.context_id == engine.tab_rename.attachment) return &engine.model.shared_mutations;
    }
    return engine.peer_edits.renameQueueForAttachment(engine.model, engine.tab_rename.attachment);
}

fn retireTabRename(engine: anytype) void {
    if (engine.tab_rename.ticket) |ticket| {
        if (tabRenameQueue(engine)) |queue| queue.forget(ticket);
    }
    engine.tab_rename = .{};
}

/// Modal displacement also abandons its waiter (without cancelling the write).
pub fn syncTabRename(engine: anytype, open: bool, target: []const u8) void {
    if (!open or !engine.tab_rename.matches(target)) retireTabRename(engine);
}

fn describeTab(engine: anytype, bytes: []const u8) Reply {
    retireTabRename(engine);
    if (comptime !support.phux_enabled) return tab_unavailable;
    const target = tab_commands.decodeCaptured(bytes) orelse return tab_changed;
    const index = target.resolve(engine.model) orelse return tab_changed;
    const ws = engine.model.wsAtConst(target.window) orelse return tab_changed;
    const tree = ws.treeConst(index) orelse return tab_changed;
    const remote = engine.model.phuxForTree(tree) orelse return tab_unavailable;
    const attachment = tree.attachment_id orelse return tab_unavailable;
    const id = ws.shared_ids[index] orelse return tab_unavailable;
    const snapshot = remote.workspaceSnapshot();
    if (remote.context_id != attachment or remote.state() != .attached) return tab_unavailable;
    const window = shared_mutations.findWindow(snapshot, id) orelse return tab_changed;
    if (window.name.len > max_text_bytes) return .{ .phase = .unavailable, .reason = "This tab name is too long to edit here." };
    engine.tab_rename = .{ .target = target, .attachment = attachment, .epoch = remote.connectionEpoch(), .session = snapshot.session_id, .window = id, .phase = .ready };
    const length = if (window.name.len > 0) blk: {
        const name = window.name.slice();
        @memcpy(engine.tab_rename.name[0..name.len], name);
        break :blk name.len;
    } else projection.tabTitleInto(engine.model, ws, index, &engine.tab_rename.name).len;
    engine.tab_rename.name_len = length;
    return .{ .phase = .ready, .name = engine.tab_rename.name[0..length], .host = hostLabel(engine.model, remote) };
}

fn tabRenameOwner(engine: anytype, bytes: []const u8) ?*support.PhuxProvider {
    if (comptime !support.phux_enabled) return null;
    const held = &engine.tab_rename;
    if (!held.matches(bytes)) return null;
    const index = held.target.?.resolve(engine.model) orelse return null;
    const ws = engine.model.wsAtConst(held.target.?.window) orelse return null;
    const tree = ws.treeConst(index) orelse return null;
    if (tree.attachment_id != held.attachment) return null;
    const id = ws.shared_ids[index] orelse return null;
    if (!std.mem.eql(u8, &id, &held.window)) return null;
    const remote = engine.model.phuxForTree(tree) orelse return null;
    if (remote.context_id != held.attachment or remote.connectionEpoch() != held.epoch) return null;
    if (remote.workspaceSnapshot().session_id != held.session or remote.state() != .attached) return null;
    return remote;
}

fn renameTab(engine: anytype, bytes: []const u8, name: []const u8) Reply {
    if (comptime !support.phux_enabled) return tab_unavailable;
    const remote = tabRenameOwner(engine, bytes) orelse return tab_changed;
    if (engine.tab_rename.ticket != null) return tabStatus(engine, bytes);
    if (std.mem.trim(u8, name, " \t\r\n").len == 0) return .{ .phase = .refused, .reason = "Enter a name for this tab." };
    for (name) |byte| if (byte < 0x20 or byte == 0x7f) return .{ .phase = .refused, .reason = "A tab name cannot contain control characters." };
    const ticket = requestTabRename(engine, remote, name) catch |err| return .{
        .phase = .refused,
        .reason = if (err == error.WorkspaceBusy) "The workspace is changing. Try again when it settles." else "Could not rename this tab on its owning host.",
    };
    engine.tab_rename.ticket = ticket;
    engine.tab_rename.phase = .pending;
    return tabStatus(engine, bytes);
}

fn requestTabRename(engine: anytype, remote: *support.PhuxProvider, name: []const u8) !u64 {
    if (engine.model.phux()) |primary| {
        if (primary == remote) return engine.model.shared_mutations.requestRename(engine.model, engine.tab_rename.window, name);
    }
    return engine.peer_edits.renameForAttachment(engine.model, remote.context_id, engine.tab_rename.window, name);
}

fn tabStatus(engine: anytype, bytes: []const u8) Reply {
    if (!engine.tab_rename.matches(bytes)) return tab_changed;
    if (engine.tab_rename.phase != .pending) return .{ .phase = engine.tab_rename.phase, .reason = if (engine.tab_rename.phase == .refused) tab_unknown.reason else "" };
    _ = tabRenameOwner(engine, bytes) orelse {
        retireTabRename(engine);
        return tab_unknown;
    };
    const queue = tabRenameQueue(engine) orelse return tab_unknown;
    const ticket = engine.tab_rename.ticket orelse return tab_unknown;
    const completion = queue.takeCompletion(ticket) orelse return .{ .phase = .pending };
    engine.tab_rename.ticket = null;
    engine.tab_rename.phase = if (completion == .confirmed) .renamed else .refused;
    return .{ .phase = engine.tab_rename.phase, .reason = if (completion == .confirmed) "" else tab_unknown.reason };
}

test "a rename request names its session; malformed requests are refused" {
    try std.testing.expectEqual(Kind.describe, (try decode(&.{ 1, 1, 0 })).kind);
    const renamed = try decode(&.{ 1, 2, 4, 's', 'h', 'i', 'p' });
    try std.testing.expectEqualStrings("ship", renamed.name);
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 2, 0 }));
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 2, 2, 0xff, 0xfe }));
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 3, 1, 'x' }));
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 2, 1, 0 }));
    // Row kinds: the name, then target_len and the target, exactly.
    const row = try decode(&.{ 1, 7, 4, 's', 'h', 'i', 'p', 2, 0xaa, 0xbb });
    try std.testing.expectEqual(Kind.rename_row, row.kind);
    try std.testing.expectEqualStrings("ship", row.name);
    try std.testing.expectEqualSlices(u8, &.{ 0xaa, 0xbb }, row.target);
    const described = try decode(&.{ 1, 6, 0, 1, 0xaa });
    try std.testing.expectEqual(Kind.describe_row, described.kind);
    try std.testing.expectEqualSlices(u8, &.{0xaa}, described.target);
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 7, 1, 'x' })); // no target_len
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 7, 1, 'x', 0 })); // empty target
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 7, 1, 'x', 2, 0xaa })); // short
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 7, 1, 'x', 1, 0xaa, 0xbb })); // trailing
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 7, 0, 1, 0xaa })); // rename needs a name
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 6, 1, 'x', 1, 0xaa })); // describe takes none
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 2, 1, 'x', 1, 0xaa })); // only row kinds carry one
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 8, 0 }));
    var longest: [3 + 255 + 1 + 255]u8 = @splat('a');
    longest[0] = 1;
    longest[1] = 7;
    longest[2] = 255;
    longest[3 + 255] = 255;
    try std.testing.expectEqual(@as(usize, 255), (try decode(&longest)).target.len);
    var out: [max_bytes]u8 = undefined;
    const bytes = try encode(.{ .phase = .refused, .name = "a", .host = "mini", .reason = "why" }, &out);
    try std.testing.expectEqualSlices(u8, &.{ 1, 3, 1, 'a', 4, 'm', 'i', 'n', 'i', 3, 'w', 'h', 'y' }, bytes);
}
