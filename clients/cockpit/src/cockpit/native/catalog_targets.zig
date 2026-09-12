//! Opaque catalog authority: full provider identity plus process-local context.
//! Placement, metadata, query and page revisions are deliberately absent.
const std = @import("std");
const model_module = @import("../model.zig");
const support = @import("../phux_support.zig");
const Model = model_module.Model;
const Entry = model_module.PaletteDestination;
const TerminalRef = model_module.TerminalRef;

pub const max_len = 34 + 9 + support.RemoteResourceId.max_host_bytes;
/// `peer_session` names a session of the peer coordinator `Target.provider_id`
/// names; its context is that provider's, so a replaced, retargeted or
/// dropped peer invalidates it. `session` is the active coordinator's.
pub const Resource = union(enum) { terminal: TerminalRef, session: u32, peer_session: u32 };

pub const Context = struct {
    provider: u64,
    host: u64 = 0,
    epoch: u64 = 0,
};

pub const Target = struct {
    provider_id: support.ProviderId,
    context: Context,
    resource: Resource,

    pub fn resolve(self: Target, model: *const Model) ?Entry {
        if (self.resource == .peer_session) return resolvePeerSession(model, self.provider_id, self.context, self.resource.peer_session);
        const current = contextFor(model, self.provider_id) orelse return null;
        if (!std.meta.eql(current, self.context)) return null;
        return switch (self.resource) {
            .terminal => |ref| if (ref.provider_id == self.provider_id) resolveTerminal(model, ref) else null,
            // A session by number is only the active coordinator's.
            .session => |id| if (activeId(model) == self.provider_id) resolveSession(model, id) else null,
            .peer_session => unreachable,
        };
    }

    pub fn encode(self: Target, out: *[max_len]u8) []const u8 {
        out[0] = 2;
        std.mem.writeInt(u64, out[2..10], @intFromEnum(self.provider_id), .little);
        std.mem.writeInt(u64, out[10..18], self.context.provider, .little);
        std.mem.writeInt(u64, out[18..26], self.context.host, .little);
        std.mem.writeInt(u64, out[26..34], self.context.epoch, .little);
        switch (self.resource) {
            .session => |id| {
                out[1] = 2;
                std.mem.writeInt(u32, out[34..38], id, .little);
                return out[0..38];
            },
            .peer_session => |id| {
                out[1] = 3;
                std.mem.writeInt(u32, out[34..38], id, .little);
                return out[0..38];
            },
            .terminal => |ref| return encodeTerminal(ref, out),
        }
    }
};

/// A coordinator's context, whichever of the held coordinators minted `id`:
/// its provider and host lifetimes and its connection. A ref of a
/// coordinator no longer held has none, so it resolves to nothing.
fn contextFor(model: *const Model, id: support.ProviderId) ?Context {
    if (id == .local) return .{ .provider = model.provider.context_id };
    if (comptime !support.phux_enabled) return null;
    const remote = model.phuxForConst(id) orelse return null;
    return .{ .provider = remote.context_id, .host = remote.host.context_id, .epoch = remote.connectionEpoch() };
}

fn activeId(model: *const Model) ?support.ProviderId {
    if (comptime !support.phux_enabled) return null;
    const remote = model.phuxConst() orelse return null;
    return remote.providerId();
}

/// A peer coordinator's context: its own provider and host lifetimes and
/// the connection its session catalog came from. Exchanging, retargeting or
/// dropping the peer changes it.
fn peerContext(model: *const Model, coordinator: support.ProviderId) ?Context {
    if (comptime !support.phux_enabled) return null;
    const slot = model.peerSlot(coordinator) orelse return null;
    const peer = model.phuxPeerAtConst(slot).?;
    return .{ .provider = peer.context_id, .host = peer.host.context_id, .epoch = peer.host.sessions_generation };
}

pub fn capture(model: *const Model, entry: Entry) ?Target {
    return switch (entry) {
        .placed_terminal => |placed| captureTerminal(model, entry, placed.terminal_ref),
        .available_terminal => |ref| captureTerminal(model, entry, ref),
        .session => |id| captureSession(model, entry, id),
        .peer_session => |target| capturePeerSession(model, target.coordinator, target.id),
        // No session is numbered 0: it names the group's retry while failed.
        .peer_unavailable => |coordinator| capturePeerSession(model, coordinator, 0),
    };
}

/// Held against the coordinator that minted the ref.
fn captureTerminal(model: *const Model, entry: Entry, ref: TerminalRef) ?Target {
    var context = contextFor(model, ref.provider_id) orelse return null;
    if (ref.provider_id != .local) context.epoch = inventoryEpoch(model, entry);
    return .{ .provider_id = ref.provider_id, .context = context, .resource = .{ .terminal = ref } };
}

/// A session by number is the active coordinator's.
fn captureSession(model: *const Model, entry: Entry, id: u32) ?Target {
    const provider_id = activeId(model) orelse return null;
    var context = contextFor(model, provider_id) orelse return null;
    context.epoch = inventoryEpoch(model, entry);
    return .{ .provider_id = provider_id, .context = context, .resource = .{ .session = id } };
}

fn capturePeerSession(model: *const Model, coordinator: support.ProviderId, id: u32) ?Target {
    const context = peerContext(model, coordinator) orelse return null;
    return .{ .provider_id = coordinator, .context = context, .resource = .{ .peer_session = id } };
}

fn inventoryEpoch(model: *const Model, entry: Entry) u64 {
    if (comptime !support.phux_enabled) return 0;
    return switch (entry) {
        .placed_terminal => |placed| blk: {
            const remote = model.phuxForRefConst(placed.terminal_ref) orelse break :blk 0;
            break :blk if (remote.owner(placed.terminal_ref)) |owner| owner.generation.epoch_id else 0;
        },
        .available_terminal => |ref| blk: {
            const remote = model.phuxForRefConst(ref) orelse break :blk 0;
            break :blk if (catalogContains(model, ref)) remote.connectionEpoch() else 0;
        },
        .session => if (model.phuxConst()) |remote| remote.host.sessions_generation else 0,
        .peer_session, .peer_unavailable => 0,
    };
}

fn placedReplicaCurrent(model: *const Model, ref: TerminalRef) bool {
    if (ref.provider_id == .local) return true;
    const remote = model.phuxForRefConst(ref) orelse return false;
    const owner = remote.owner(ref) orelse return false;
    return owner.generation.epoch_id == remote.connectionEpoch();
}

fn resolveTerminal(model: *const Model, ref: TerminalRef) ?Entry {
    if (model.locateTerminal(ref)) |location| {
        if (!model.containsTerminal(ref)) return null;
        if (!placedReplicaCurrent(model, ref)) return null;
        return .{ .placed_terminal = .{ .window = @intCast(location.window), .tab = @intCast(location.tab), .terminal_ref = ref } };
    }
    if (ref.provider_id == .local) return null;
    if (!catalogContains(model, ref)) return null;
    return .{ .available_terminal = ref };
}

fn catalogContains(model: *const Model, ref: TerminalRef) bool {
    const remote = model.phuxForRefConst(ref) orelse return false;
    for (remote.catalogTerminals()) |terminal| {
        if (terminal.terminal_ref.eql(ref)) return true;
    }
    return false;
}

fn resolveSession(model: *const Model, id: u32) ?Entry {
    if (comptime !support.phux_enabled) return null;
    const remote = model.phuxConst() orelse return null;
    if (remote.host.sessions_generation != remote.connectionEpoch()) return null;
    for (remote.sessionCatalog()) |session| {
        if (session.id == id) return .{ .session = id };
    }
    return null;
}

/// Held against the peer that listed it: exchanging, retargeting or dropping
/// that peer, or a catalog from an older connection of it, refuses. Shown by
/// id, so an unnamed session is as selectable as a named one.
fn resolvePeerSession(model: *const Model, coordinator: support.ProviderId, expected: Context, id: u32) ?Entry {
    if (comptime !support.phux_enabled) return null;
    const current = peerContext(model, coordinator) orelse return null;
    if (!std.meta.eql(current, expected)) return null;
    const slot = model.peerSlot(coordinator) orelse return null;
    // Session 0 is the group's unavailable row: it retries a failed peer.
    if (id == 0) return if (model.peers.items[slot].failed) .{ .peer_unavailable = coordinator } else null;
    const peer = model.phuxPeerAtConst(slot).?;
    // A peer that disconnected, is being retargeted, or listed on an older
    // connection offers nothing; forgetting also moved the context above.
    for (peer.standbyCatalog()) |session| {
        if (session.id == id) return .{ .peer_session = .{ .coordinator = coordinator, .id = id } };
    }
    return null;
}

fn encodeTerminal(ref: TerminalRef, out: *[max_len]u8) []const u8 {
    switch (ref.terminal_id) {
        .local => |id| {
            out[1] = 0;
            std.mem.writeInt(u64, out[34..42], @intFromEnum(id), .little);
            return out[0..42];
        },
        .phux => |id| {
            out[1] = 1;
            std.mem.writeInt(u32, out[34..38], id.kind, .little);
            std.mem.writeInt(u32, out[38..42], id.id, .little);
            out[42] = id.host_len;
            @memcpy(out[43..][0..id.host_len], id.host());
            return out[0 .. 43 + @as(usize, id.host_len)];
        },
    }
}

pub fn decode(bytes: []const u8) ?Target {
    if (bytes.len < 38 or bytes.len > max_len) return null;
    if (bytes[0] != 2) return null;
    const provider_id: support.ProviderId = @enumFromInt(std.mem.readInt(u64, bytes[2..10], .little));
    return .{
        .provider_id = provider_id,
        .context = .{
            .provider = std.mem.readInt(u64, bytes[10..18], .little),
            .host = std.mem.readInt(u64, bytes[18..26], .little),
            .epoch = std.mem.readInt(u64, bytes[26..34], .little),
        },
        .resource = decodeResource(bytes, provider_id) orelse return null,
    };
}

fn decodeResource(bytes: []const u8, provider_id: support.ProviderId) ?Resource {
    switch (bytes[1]) {
        0 => {
            if (provider_id != .local or bytes.len != 42) return null;
            return .{ .terminal = .{ .provider_id = provider_id, .terminal_id = .{ .local = @enumFromInt(std.mem.readInt(u64, bytes[34..42], .little)) } } };
        },
        1 => return decodeRemote(bytes, provider_id),
        // Any Phux coordinator; resolution then refuses one no longer held.
        2 => {
            if (!isCoordinator(provider_id) or bytes.len != 38) return null;
            return .{ .session = std.mem.readInt(u32, bytes[34..38], .little) };
        },
        3 => {
            if (!isCoordinator(provider_id) or bytes.len != 38) return null;
            return .{ .peer_session = std.mem.readInt(u32, bytes[34..38], .little) };
        },
        else => return null,
    }
}

fn isCoordinator(id: support.ProviderId) bool {
    return @import("provider_contract").isPhuxCoordinator(id);
}

fn decodeRemote(bytes: []const u8, provider_id: support.ProviderId) ?Resource {
    if (!isCoordinator(provider_id) or bytes.len < 43) return null;
    if (bytes.len != 43 + @as(usize, bytes[42])) return null;
    const id = support.RemoteResourceId.fromPhux(
        std.mem.readInt(u32, bytes[34..38], .little),
        std.mem.readInt(u32, bytes[38..42], .little),
        bytes[43..],
    ) catch return null;
    return .{ .terminal = .{ .provider_id = provider_id, .terminal_id = .{ .phux = id } } };
}

test "navigation opaque catalog targets preserve full identity and bounded host bytes" {
    const host = [_]u8{0xfe} ** support.RemoteResourceId.max_host_bytes;
    const target: Target = .{
        .provider_id = .phux,
        .context = .{ .provider = 0xfedc_ba98_7654_3210, .host = 0xabcd_ef01_2345_6789, .epoch = 0xffff_ffff_ffff_fffe },
        .resource = .{ .terminal = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0xfedc_ba98, 0xabcd_ef01, &host) } } },
    };
    var out: [max_len]u8 = undefined;
    const bytes = target.encode(&out);
    try std.testing.expectEqual(max_len, bytes.len);
    const roundtrip = decode(bytes).?;
    try std.testing.expectEqual(target.provider_id, roundtrip.provider_id);
    try std.testing.expectEqual(target.context, roundtrip.context);
    try std.testing.expect(target.resource.terminal.eql(roundtrip.resource.terminal));
    for (0..bytes.len) |len| try std.testing.expect(decode(bytes[0..len]) == null);
    var excess: [max_len + 1]u8 = undefined;
    @memcpy(excess[0..max_len], bytes);
    excess[max_len] = 0;
    try std.testing.expect(decode(&excess) == null);
    out[42] -= 1;
    try std.testing.expect(decode(&out) == null);
    out[42] += 1;
    // Same resource under another coordinator never aliases: it decodes as
    // that coordinator's terminal, not this one's.
    const mini = @import("provider_contract").phuxCoordinatorId("mini");
    std.mem.writeInt(u64, out[2..10], @intFromEnum(mini), .little);
    const other = decode(&out).?;
    try std.testing.expectEqual(mini, other.provider_id);
    try std.testing.expect(!other.resource.terminal.eql(target.resource.terminal));
    // An id no coordinator can have, and a local id, carry no Phux resource.
    std.mem.writeInt(u64, out[2..10], @intFromEnum(support.ProviderId.phux) ^ 1, .little);
    try std.testing.expect(decode(&out) == null);
    std.mem.writeInt(u64, out[2..10], @intFromEnum(support.ProviderId.local), .little);
    try std.testing.expect(decode(&out) == null);

    const local: Target = .{ .provider_id = .local, .context = target.context, .resource = .{ .terminal = .{
        .provider_id = .local,
        .terminal_id = .{ .local = @enumFromInt(0xfedc_ba98_7654_3210) },
    } } };
    try std.testing.expect(local.resource.terminal.eql(decode(local.encode(&out)).?.resource.terminal));
    const session: Target = .{ .provider_id = .phux, .context = target.context, .resource = .{ .session = 0xffff_fffe } };
    try std.testing.expectEqual(session.resource.session, decode(session.encode(&out)).?.resource.session);
}

test "navigation catalog context rejects provider host and connection replacement" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try @import("ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const remote = try model_module.PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/catalog-context-unused" }, null, "test");
    engine.model.phux_provider = remote;
    try remote.host.sessions.append(std.testing.allocator, .{
        .id = 0xffff_fffe,
        .name = try std.testing.allocator.dupe(u8, "session"),
        .created_at_unix_secs = 0,
        .window_count = 1,
        .attached_client_count = 0,
        .focused = false,
    });
    remote.host.sessions_generation = remote.connectionEpoch();
    const target = capture(engine.model, .{ .session = 0xffff_fffe }).?;
    try std.testing.expectEqual(@as(u32, 0xffff_fffe), target.resolve(engine.model).?.session);
    const replacement = try model_module.PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/catalog-context-unused" }, null, "test");
    defer replacement.destroy();
    engine.model.phux_provider = replacement;
    try std.testing.expect(target.resolve(engine.model) == null);
    engine.model.phux_provider = remote;
    const host = remote.host;
    remote.host = replacement.host;
    try std.testing.expect(target.resolve(engine.model) == null);
    remote.host = host;
    remote.host.client_generation += 1;
    try std.testing.expect(target.resolve(engine.model) == null);
    remote.host.client_generation -= 1;
    try std.testing.expect(target.resolve(engine.model) != null);
    remote.host.sessions.items[0].id = 7;
    try std.testing.expect(target.resolve(engine.model) == null);
}
