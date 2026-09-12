//! Provider-neutral terminal identity and presentation values.
//!
//! This module is deliberately transport- and ABI-independent. UI placement is
//! never part of an execution identity: a `TerminalRef` remains stable while a
//! terminal moves between tabs or split panes, and a `ReplicaOwner` additionally
//! fences work to one published provider generation.

const std = @import("std");
pub const workspace = @import("workspace.zig");
pub const context = @import("context.zig");
const native_sdk = @import("native_sdk");

const canvas = native_sdk.canvas;

/// Stable identity of a provider instance. The local provider has one fixed
/// identity; the Phux provider keeps its identity across reconnects.
pub const ProviderId = enum(u64) {
    local = 0x6c6f_6361_6c00_0001,
    phux = 0x7068_7578_0000_0001,
    _,
};

const remote_coordinator_base: u64 = 0x7068_7578_8000_0000;

/// The identity of one Phux coordinator, derived from where it is reached so
/// it is stable across reconnects and relaunches. This Mac's coordinator (a
/// socket) is `.phux`, which every saved placement from before several
/// coordinators existed already names. A registered remote host is keyed by
/// its registry target, with bit 31 set so it can never equal `.phux` or
/// `.local`. Terminal ids are allocated per server, so two coordinators can
/// both publish terminal 7; the provider id is what keeps those two apart in
/// every placement, target and input route.
pub fn phuxCoordinatorId(remote_target: ?[]const u8) ProviderId {
    const target = remote_target orelse return .phux;
    const mixed = std.hash.Wyhash.hash(remote_coordinator_base, target);
    return @enumFromInt(remote_coordinator_base | (mixed & 0x7fff_ffff));
}

/// Whether an id read from outside (a saved placement, a catalog target)
/// can name a Phux coordinator at all: this Mac's, or the remote form
/// `phuxCoordinatorId` produces. Anything else is refused on decode.
pub fn isPhuxCoordinator(id: ProviderId) bool {
    if (id == .phux) return true;
    return (@intFromEnum(id) & 0xffff_ffff_8000_0000) == remote_coordinator_base;
}

/// Durable identities owned by the built-in local provider.
pub const LocalResourceId = enum(u64) {
    terminal_1 = 0x7465_726d_0000_0001,
    terminal_2 = 0x7465_726d_0000_0002,
    _,
};

pub const RemoteResourceIdError = error{HostTooLong};

/// Durable Phux execution identity. `host_storage` is inline so identity values
/// never borrow an FFI frame and never allocate. Hosts longer than the protocol
/// maximum are rejected rather than truncated into a collision.
pub const RemoteResourceId = struct {
    pub const max_host_bytes: usize = 255;

    kind: u32,
    id: u32,
    host_storage: [max_host_bytes]u8 = [_]u8{0} ** max_host_bytes,
    host_len: u8 = 0,

    pub fn fromPhux(kind: u32, id: u32, host_name: []const u8) RemoteResourceIdError!RemoteResourceId {
        if (host_name.len > max_host_bytes) return error.HostTooLong;
        var result: RemoteResourceId = .{ .kind = kind, .id = id };
        @memcpy(result.host_storage[0..host_name.len], host_name);
        result.host_len = @intCast(host_name.len);
        return result;
    }

    pub fn host(remote: *const RemoteResourceId) []const u8 {
        return remote.host_storage[0..remote.host_len];
    }

    pub fn eql(a: RemoteResourceId, b: RemoteResourceId) bool {
        return a.kind == b.kind and a.id == b.id and std.mem.eql(u8, a.host(), b.host());
    }

    pub fn hash(remote: RemoteResourceId) u64 {
        var hasher = std.hash.Wyhash.init(0);
        hasher.update(std.mem.asBytes(&remote.kind));
        hasher.update(std.mem.asBytes(&remote.id));
        hasher.update(remote.host());
        return hasher.final();
    }
};

/// Provider-owned terminal identity. It is not sufficient for UI routing until
/// qualified by the owning provider in `TerminalRef`.
pub const ResourceId = union(enum) {
    local: LocalResourceId,
    phux: RemoteResourceId,

    pub fn eql(a: ResourceId, b: ResourceId) bool {
        return switch (a) {
            .local => |id| switch (b) {
                .local => |other| id == other,
                .phux => false,
            },
            .phux => |id| switch (b) {
                .local => false,
                .phux => |other| id.eql(other),
            },
        };
    }

    pub fn hash(id: ResourceId) u64 {
        return switch (id) {
            .local => |local| mixHash(0, @intFromEnum(local)),
            .phux => |remote| mixHash(1, remote.hash()),
        };
    }
};

/// The sole identity accepted by provider lookup and stored in placements.
pub const TerminalRef = struct {
    provider_id: ProviderId,
    terminal_id: ResourceId,

    pub fn eql(a: TerminalRef, b: TerminalRef) bool {
        return a.provider_id == b.provider_id and a.terminal_id.eql(b.terminal_id);
    }

    pub fn hash(terminal_ref: TerminalRef) u64 {
        return mixHash(@intFromEnum(terminal_ref.provider_id), terminal_ref.terminal_id.hash());
    }
};

/// Hash-map context for provider-qualified identities.
pub const TerminalRefContext = struct {
    pub fn hash(_: TerminalRefContext, terminal_ref: TerminalRef) u64 {
        return terminal_ref.hash();
    }

    pub fn eql(_: TerminalRefContext, a: TerminalRef, b: TerminalRef) bool {
        return a.eql(b);
    }
};

/// Published provider generation. `last_seq` is progress within a replica, not
/// replica identity; reconnect fences compare host epoch, stream, and bootstrap.
pub const Generation = struct {
    epoch_id: u64 = 0,
    stream_id: u64 = 0,
    bootstrap_id: u64 = 0,
    last_seq: u64 = 0,

    pub fn sameReplica(a: Generation, b: Generation) bool {
        return a.epoch_id == b.epoch_id and a.stream_id == b.stream_id and a.bootstrap_id == b.bootstrap_id;
    }
};

/// A provider-qualified terminal pinned to one replica generation.
pub const ReplicaOwner = struct {
    terminal_ref: TerminalRef,
    generation: Generation,
    /// Process-local source identity: independent connections may publish the
    /// same resource and wire generation. Zero is the local/legacy default.
    source_context: u64 = 0,

    /// Owner equality deliberately excludes `last_seq`: applying more frames to
    /// the same replica must not invalidate a held key or asynchronous result.
    pub fn eql(a: ReplicaOwner, b: ReplicaOwner) bool {
        return a.source_context == b.source_context and a.terminal_ref.eql(b.terminal_ref) and a.generation.sameReplica(b.generation);
    }
};

pub const Phase = enum {
    starting,
    attaching,
    live,
    reconnecting,
    tombstoned,
    frozen,
    ended,
    failed,
};

/// Whether scrollback search matches case-sensitively. ONE rule for every
/// provider, stated once, because cmd+F is one chord and a chord that matches
/// differently depending on which kind of terminal happens to be focused is
/// the sort of inconsistency nobody notices until it wastes their time.
///
/// False because the local side cannot currently be anything else: the pinned
/// `vt.search` matches case-insensitively with no option to change it (see
/// `sliding_window.zig`, whose matcher is ASCII case-insensitive throughout).
/// The remote phux side CAN do either, so it is the one that has to be told,
/// and it reads this rather than taking a caller's bool.
///
/// A case TOGGLE is a legitimate future feature. It cannot be built by letting
/// the two sides diverge here; it needs case sensitivity in the engine first,
/// at which point this becomes a default rather than a constant. See
/// phux-cockpit-kxw.
pub const search_case_sensitive: bool = false;

pub const PixelSize = struct {
    width: u16,
    height: u16,
};

pub const Viewport = struct {
    cols: u16,
    rows: u16,
    pixels: ?PixelSize = null,

    pub fn eql(a: Viewport, b: Viewport) bool {
        if (a.cols != b.cols or a.rows != b.rows) return false;
        if (a.pixels == null or b.pixels == null) return a.pixels == null and b.pixels == null;
        return a.pixels.?.width == b.pixels.?.width and a.pixels.?.height == b.pixels.?.height;
    }
};

pub const KeyAction = enum { press, repeat, release };
pub const PhysicalKey = enum(u32) { _ };

/// Provider-neutral modifier bits. The transport adapter translates these to
/// its wire/FFI representation; this module contains no C declarations.
pub const ModifierMask = packed struct(u16) {
    shift: bool = false,
    control: bool = false,
    alt: bool = false,
    super: bool = false,
    caps_lock: bool = false,
    num_lock: bool = false,
    _padding: u10 = 0,
};

pub const KeyInput = struct {
    action: KeyAction,
    physical: PhysicalKey,
    modifiers: ModifierMask = .{},
    text: []const u8 = &.{},
    composing: bool = false,
    unshifted_codepoint: ?u21 = null,
};

pub const MouseAction = enum { press, release, move };
pub const MouseButton = enum(u8) { none, left, middle, right, button_4, button_5, button_6, button_7, _ };
pub const MouseMode = enum(u32) { off, x10, normal, button, any_motion };
pub const MeasuredCell = struct { width: f32, height: f32 };
pub const SelectionGesture = struct {
    phase: enum(u32) { press, drag, release },
    clicks: u8 = 1,
    handle: u64 = 0,
    cell: DocumentPoint,
    x: f64,
    y: f64,
    columns: u16,
    cell_width: f32,
    screen_height: f32,
    rectangle: bool = false,
};
pub const SelectionGestureResult = struct { handle: u64, start: u64, end: u64 };

pub const MouseInput = struct {
    action: MouseAction,
    button: MouseButton = .none,
    modifiers: ModifierMask = .{},
    x: f64,
    y: f64,
};

pub const ScrollKind = enum { delta, top, bottom };
pub const Scroll = struct {
    kind: ScrollKind,
    value: i64 = 0,
};
pub const DocumentSpace = enum(u32) { history = 0, viewport = 1, active = 2 };
pub const DocumentPoint = struct {
    space: DocumentSpace,
    row: u32,
    column: u16,
};

/// Provider-owned, non-authoritative rendering projection. All borrowed slices
/// and the grid remain valid through painting or until the next provider
/// mutation, whichever comes first.
pub const Presentation = struct {
    measured_cell: ?MeasuredCell = null,
    grid: canvas.TerminalGrid,
    owner: ReplicaOwner,
    phase: Phase,
    title: []const u8,
    cols: u16,
    rows: u16,
    history_total_rows: u64 = 0,
    history_viewport_offset: u64 = 0,
    history_visible_rows: u64 = 0,
    history_loading: bool = false,
    history_has_more: bool = false,
    history_pages_loaded: u64 = 0,
    history_unread_rows: u64 = 0,
};

pub fn localResourceId(index: usize) LocalResourceId {
    return if (index == 0) .terminal_1 else .terminal_2;
}

pub fn localTerminalRef(id: LocalResourceId) TerminalRef {
    return .{ .provider_id = .local, .terminal_id = .{ .local = id } };
}

pub fn localTerminalRefForIndex(index: usize) TerminalRef {
    return localTerminalRef(localResourceId(index));
}

pub fn localGeneration(spawn_generation: u64) Generation {
    return .{
        .stream_id = @intFromEnum(ProviderId.local),
        .bootstrap_id = spawn_generation,
    };
}

pub fn localReplicaOwner(terminal_ref: TerminalRef, spawn_generation: u64) ReplicaOwner {
    return .{
        .terminal_ref = terminal_ref,
        .generation = localGeneration(spawn_generation),
    };
}

pub fn localId(terminal_ref: TerminalRef) ?LocalResourceId {
    if (terminal_ref.provider_id != .local) return null;
    return switch (terminal_ref.terminal_id) {
        .local => |id| id,
        .phux => null,
    };
}

pub fn isLocal(terminal_ref: TerminalRef) bool {
    return localId(terminal_ref) != null;
}

fn mixHash(a: u64, b: u64) u64 {
    var hasher = std.hash.Wyhash.init(0);
    hasher.update(std.mem.asBytes(&a));
    hasher.update(std.mem.asBytes(&b));
    return hasher.final();
}

test "replica identity includes provider epoch stream and bootstrap" {
    const base: Generation = .{ .epoch_id = 1, .stream_id = 2, .bootstrap_id = 3, .last_seq = 4 };
    try std.testing.expect(base.sameReplica(.{ .epoch_id = 1, .stream_id = 2, .bootstrap_id = 3, .last_seq = 99 }));
    try std.testing.expect(!base.sameReplica(.{ .epoch_id = 9, .stream_id = 2, .bootstrap_id = 3 }));
    try std.testing.expect(!base.sameReplica(.{ .epoch_id = 1, .stream_id = 9, .bootstrap_id = 3 }));
    try std.testing.expect(!base.sameReplica(.{ .epoch_id = 1, .stream_id = 2, .bootstrap_id = 9 }));
}

test "coordinator identity separates hosts and keeps this Mac's saved id" {
    try std.testing.expectEqual(ProviderId.phux, phuxCoordinatorId(null));
    const mini = phuxCoordinatorId("mini");
    try std.testing.expectEqual(mini, phuxCoordinatorId("mini"));
    try std.testing.expect(mini != .phux and mini != .local);
    try std.testing.expect(mini != phuxCoordinatorId("studio"));
    try std.testing.expect(isPhuxCoordinator(.phux) and isPhuxCoordinator(mini));
    try std.testing.expect(!isPhuxCoordinator(.local));
    try std.testing.expect(!isPhuxCoordinator(@enumFromInt(1)));
    // The same numeric terminal on two coordinators is two identities.
    const id: RemoteResourceId = .{ .kind = 0, .id = 7 };
    const here: TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = id } };
    const there: TerminalRef = .{ .provider_id = mini, .terminal_id = .{ .phux = id } };
    try std.testing.expect(!here.eql(there));
    try std.testing.expect(here.hash() != there.hash());
}

test "viewport equality includes pixel geometry" {
    const base: Viewport = .{ .cols = 80, .rows = 24, .pixels = .{ .width = 800, .height = 480 } };
    try std.testing.expect(base.eql(base));
    try std.testing.expect(!base.eql(.{ .cols = 80, .rows = 24, .pixels = .{ .width = 801, .height = 480 } }));
    try std.testing.expect(!base.eql(.{ .cols = 80, .rows = 24 }));
}
