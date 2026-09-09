//! Provider-owned agent inspection over the existing fenced navigation seam.
const std = @import("std");
const model_module = @import("../model.zig");
const support = @import("../phux_support.zig");
const projection = @import("workspace_projection.zig");
const navigation = @import("ts_navigation.zig");

pub const no_parent: u16 = 65535;
pub const max_identity_bytes = 288;
/// Full provider identity (256), retained reason (1024), and a 512-byte
/// envelope for state/type labels and two lossless decimal u64 values.
/// Keep the fourth-field bound in protocol.ts in sync.
pub const max_evidence_bytes = 256 + 1024 + 512;
pub const max_snapshot_rows = 24;
pub const Parent = struct { index: u16 = no_parent, window: u8 = 255, tab: u8 = 255 };

pub fn total(model: *const model_module.Model) usize {
    if (comptime !support.phux_enabled) return 0;
    const remote = model.phuxConst() orelse return 0;
    return remote.agentSessions().len;
}

pub fn identity(ref: model_module.TerminalRef, out: []u8) []const u8 {
    const id = ref.terminal_id.phux;
    return std.fmt.bufPrint(out, "phux:{d}:{d}@{s}", .{ id.kind, id.id, id.host() }) catch unreachable;
}

/// The index addresses a terminal, never an agent replica. Navigation resolves
/// this same catalog again only after checking the revision fence.
pub fn parentTarget(model: *const model_module.Model, ref: ?model_module.TerminalRef) Parent {
    const wanted = ref orelse return .{};
    const empty: model_module.Workspace = .{};
    var entries: [projection.palette_max_visible_rows]projection.PaletteEntry = undefined;
    var first: usize = 0;
    while (first < no_parent) : (first += entries.len) {
        const count = projection.paletteEntriesWindowIn(model, &empty, .{ .first = first, .count = entries.len }, &entries);
        for (entries[0..count], first..) |entry, index| {
            if (targetFor(entry, wanted, index)) |target| return target;
        }
        if (count < entries.len) break;
    }
    return .{};
}

fn targetFor(entry: projection.PaletteEntry, wanted: model_module.TerminalRef, index: usize) ?Parent {
    if (index >= no_parent) return null;
    switch (entry) {
        .placed_terminal => |placed| {
            if (!placed.terminal_ref.eql(wanted)) return null;
            return .{ .index = @intCast(index), .window = placed.window, .tab = @intCast(placed.tab) };
        },
        .available_terminal => |ref| {
            if (ref.eql(wanted)) return .{ .index = @intCast(index) };
        },
        // Sessions and peer rows address coordinators, never a terminal.
        .session, .peer_session, .peer_unavailable => {},
    }
    return null;
}

fn field(out: []u8, start: usize, value: []const u8) navigation.Error!usize {
    if (start + 2 + value.len > out.len) return error.BufferTooSmall;
    std.mem.writeInt(u16, out[start..][0..2], @intCast(value.len), .little);
    @memcpy(out[start + 2 ..][0..value.len], value);
    return start + 2 + value.len;
}

fn optionalNumber(value: ?u64, out: *[20]u8) []const u8 {
    const number = value orelse return "unknown";
    return std.fmt.bufPrint(out, "{d}", .{number}) catch unreachable;
}

const missing_evidence_text =
    \\Latest record: not observed
    \\Sequence: unknown
    \\Coordinator-stamped record time (ts_ms): unknown
    \\Reason: not observed
;

const latest_evidence_format =
    \\Latest record: {s}
    \\Sequence: {s}
    \\Coordinator-stamped record time (ts_ms): {s}
    \\Reason{s}: {s}
;

fn latestEvidenceText(session: *const model_module.AgentSession, out: []u8) navigation.Error![]const u8 {
    if (comptime !support.phux_enabled) return "";
    const latest = session.latest_evidence orelse return std.fmt.bufPrint(out, missing_evidence_text, .{}) catch error.BufferTooSmall;
    var sequence: [20]u8 = undefined;
    var timestamp: [20]u8 = undefined;
    const reason = latest.reason.slice();
    return std.fmt.bufPrint(out, latest_evidence_format, .{
        latest.record_type,
        optionalNumber(latest.seq, &sequence),
        optionalNumber(latest.ts_ms, &timestamp),
        if (latest.reason.truncated) " (truncated)" else "",
        if (reason.len == 0) "not supplied" else reason,
    }) catch error.BufferTooSmall;
}

fn evidenceText(session: *const model_module.AgentSession, out: []u8) navigation.Error![]const u8 {
    if (comptime !support.phux_enabled) return "";
    const source = if (session.stream_state) |state| state.word() else "not observed";
    const header = std.fmt.bufPrint(out, "Provider: {s}\nCatalog: {s}; records: {s}\n", .{
        session.provider_name, session.catalog_state.word(), source,
    }) catch return error.BufferTooSmall;
    const latest = try latestEvidenceText(session, out[header.len..]);
    return out[0 .. header.len + latest.len];
}

fn encodeInspection(model: *const model_module.Model, session: *const model_module.AgentSession, out: []u8, start: usize) navigation.Error!usize {
    if (comptime !support.phux_enabled) return start;
    const target = parentTarget(model, session.parentRef());
    var label_buffer: [640]u8 = undefined;
    var label_display: [navigation.max_label_bytes]u8 = undefined;
    const label = navigation.displayText(std.fmt.bufPrint(&label_buffer, "{s} · {s}", .{ session.provider_name, session.state().word() }) catch "Agent", &label_display);
    if (start + 3 + label.len > out.len) return error.BufferTooSmall;
    std.mem.writeInt(u16, out[start..][0..2], target.index, .little);
    out[start + 2] = @intCast(label.len);
    @memcpy(out[start + 3 ..][0..label.len], label);
    var resource_buffer: [max_identity_bytes]u8 = undefined;
    var parent_buffer: [max_identity_bytes]u8 = undefined;
    var written = try field(out, start + 3 + label.len, identity(session.ref(), &resource_buffer));
    written = try field(out, written, if (session.parentRef()) |ref| identity(ref, &parent_buffer) else "No parent reported");
    written = try field(out, written, session.native_id);
    var evidence: [max_evidence_bytes]u8 = undefined;
    return field(out, written, try evidenceText(session, &evidence));
}

/// Kind 5: one complete inspection row per page. The usual navigation header
/// echoes the revision and offset, so delayed replies cannot replace a page.
pub fn encode(model: *const model_module.Model, revision: u64, request: []const u8, out: []u8) navigation.Error![]const u8 {
    if (request.len != 13 or request[0] != 1 or request[1] != 5 or request[12] != 0) return error.InvalidRequest;
    if (std.mem.readInt(u64, request[2..10], .little) != revision) return error.StaleRevision;
    const count = total(model);
    const first = std.mem.readInt(u16, request[10..12], .little);
    if (first > count) return error.InvalidRequest;
    if (out.len < 16) return error.BufferTooSmall;
    @memcpy(out[0..13], request);
    std.mem.writeInt(u16, out[13..15], @intCast(count), .little);
    out[15] = 0;
    if (first == count) return out[0..16];
    if (comptime !support.phux_enabled) return out[0..16];
    const session = &model.phuxConst().?.agentSessions()[first];
    const end = try encodeInspection(model, session, out, 16);
    out[15] = 1;
    return out[0..end];
}

fn snapshotRow(model: *const model_module.Model, session: *const model_module.AgentSession, out: []u8, start: usize) ?usize {
    if (comptime !support.phux_enabled) return null;
    const parent = session.parentRef() orelse return null;
    if (model.locateTerminal(parent) == null) return null;
    const target = parentTarget(model, parent);
    if (target.window == 255) return null;
    var resource_buffer: [max_identity_bytes]u8 = undefined;
    var parent_buffer: [max_identity_bytes]u8 = undefined;
    var provider_buffer: [12]u8 = undefined;
    const resource = identity(session.ref(), &resource_buffer);
    const owner = identity(parent, &parent_buffer);
    const provider = navigation.displayText(session.provider_name, &provider_buffer);
    const end = start + 11 + resource.len + owner.len + provider.len;
    if (end > out.len) return null;
    out[start] = target.window;
    out[start + 1] = target.tab;
    std.mem.writeInt(u16, out[start + 2 ..][0..2], target.index, .little);
    out[start + 4] = @intFromEnum(session.state());
    out[start + 5] = if (session.state().needsAttention() and navigation.connection(model) == .connected) 1 else 0;
    out[start + 6] = @intCast(provider.len);
    std.mem.writeInt(u16, out[start + 7 ..][0..2], @intCast(resource.len), .little);
    std.mem.writeInt(u16, out[start + 9 ..][0..2], @intCast(owner.len), .little);
    var at = start + 11;
    for ([_][]const u8{ provider, resource, owner }) |value| {
        @memcpy(out[at..][0..value.len], value);
        at += value.len;
    }
    return end;
}

/// Remaining capacity bounds the drawn prefix. The complete count and kind-5
/// inspector guarantee every omitted row remains reachable.
pub fn snapshot(model: *const model_module.Model, out: []u8, start: usize) navigation.Error!usize {
    const count = total(model);
    if (count == 0) return start;
    if (start + 6 > out.len) return error.BufferTooSmall;
    out[start] = @intFromEnum(@import("ts_snapshot.zig").ExtensionKind.parent_agent_rows);
    std.mem.writeInt(u16, out[start + 3 ..][0..2], @intCast(count), .little);
    out[start + 5] = 0;
    var written = start + 6;
    if (comptime support.phux_enabled) {
        for (model.phuxConst().?.agentSessions()) |*session| {
            if (out[start + 5] == max_snapshot_rows) break;
            written = snapshotRow(model, session, out, written) orelse continue;
            out[start + 5] += 1;
        }
    }
    std.mem.writeInt(u16, out[start + 1 ..][0..2], @intCast(written - start - 3), .little);
    return written;
}
