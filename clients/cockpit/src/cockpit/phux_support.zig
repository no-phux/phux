const std = @import("std");
const native_sdk = @import("native_sdk");
const provider_contract = @import("provider_contract");
const phux_options = @import("phux_options");

pub const ProviderId = provider_contract.ProviderId;
pub const LocalResourceId = provider_contract.LocalResourceId;
pub const RemoteResourceId = provider_contract.RemoteResourceId;
pub const ResourceId = provider_contract.ResourceId;
pub const TerminalRef = provider_contract.TerminalRef;
pub const Generation = provider_contract.Generation;
pub const ReplicaOwner = provider_contract.ReplicaOwner;
pub const PixelSize = provider_contract.PixelSize;
pub const Viewport = provider_contract.Viewport;
pub const KeyAction = provider_contract.KeyAction;
pub const PhysicalKey = provider_contract.PhysicalKey;
pub const ModifierMask = provider_contract.ModifierMask;
pub const KeyInput = provider_contract.KeyInput;
pub const MouseAction = provider_contract.MouseAction;
pub const MouseButton = provider_contract.MouseButton;
pub const MouseInput = provider_contract.MouseInput;
pub const ScrollKind = provider_contract.ScrollKind;
pub const Scroll = provider_contract.Scroll;
pub const Presentation = provider_contract.Presentation;
pub const Phase = provider_contract.Phase;

pub const phux_enabled = phux_options.enabled;

/// The derived lifecycle vocabulary, restated for the build with no Phux in
/// it. Same words, no derivation: without a provider there is no resource
/// catalog and no record stream to derive from. Named apart from the exported
/// alias below so a reference inside either is never ambiguous.
const DisabledAgentState = enum(u8) {
    unknown,
    working,
    blocked,
    done,
    gone,

    pub fn word(_: DisabledAgentState) []const u8 {
        return "unknown";
    }

    pub fn needsAttention(_: DisabledAgentState) bool {
        return false;
    }
};

const DisabledAgentSession = struct {
    id: RemoteResourceId = .{ .kind = 0, .id = 0 },
    parent: ?RemoteResourceId = null,
    provider_name: []const u8 = "",
    native_id: []const u8 = "",
    catalog_state: DisabledAgentState = .unknown,
    stream_state: ?DisabledAgentState = null,
    generation: ?Generation = null,
    latest_evidence: ?struct {
        seq: ?u64 = null,
        ts_ms: ?u64 = null,
        record_type: []const u8 = "",
        reason: struct {
            truncated: bool = false,

            pub fn slice(_: *const @This()) []const u8 {
                return "";
            }
        } = .{},
    } = null,

    pub fn state(_: *const DisabledAgentSession) DisabledAgentState {
        return .unknown;
    }

    pub fn ref(_: *const DisabledAgentSession) TerminalRef {
        return .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 0 } } };
    }

    pub fn parentRef(_: *const DisabledAgentSession) ?TerminalRef {
        return null;
    }
};

/// The connection host the disabled provider names: only the identity field
/// session navigation compares, never a connection.
const DisabledHost = struct {
    context_id: u64 = 0,
    sessions_generation: u64 = 0,
};
const disabled_host: DisabledHost = .{};

/// The build with no Phux in it. `create` always refuses, so no value of this
/// type is ever held and `Model.phux()` is null; every member below exists
/// only so provider-generic code compiles, and each one refuses or reports
/// nothing. Callers whose return type itself differs gate on `phux_enabled`
/// instead. Fields mirror the ones generic code reads off a provider.
const DisabledPhuxProvider = struct {
    context_id: u64 = 0,
    host: *const DisabledHost = &disabled_host,
    session_id: ?u32 = null,
    pending_retarget: ?void = null,

    pub const AgentState = DisabledAgentState;
    pub const AgentSession = DisabledAgentSession;
    pub const OperationResult = struct {
        request_id: u32,
        connection_epoch: u64,
        kind: enum { spawn, attach, detach, kill_if },
        status: enum { success, refused, unknown_outcome },
        terminal_ref: ?TerminalRef,
        error_domain: enum { none, spawn, protocol },
        error_code: u32,
        instance: ?[16]u8 = null,

        pub fn message(_: *const @This()) []const u8 {
            return "";
        }
    };
    pub const Endpoint = union(enum) {
        tcp: struct { host: []const u8, port: u16 },
        unix: []const u8,
        remote: struct { target: []const u8, config_path: []const u8 = "", status: ?*anyopaque = null },
    };
    const State = enum { new, attached };
    const Anchor = struct { opaque_id: u64 = 0 };
    pub const SessionSummary = struct {
        id: u32,
        name: []u8,
        created_at_unix_secs: i64,
        window_count: u16,
        attached_client_count: u16,
        focused: bool,
        keep_empty: bool = false,
        empty: bool = false,
    };
    const SyncDelta = struct {
        metadata_changed: bool = false,
        directory_changed: bool = false,
        sessions_listed: bool = false,
        sessions_renamed: bool = false,
        ready_published: bool = false,
        generation_changed: bool = false,
        detached: bool = false,
        added_count: usize = 0,
        removed_count: usize = 0,
    };

    pub fn create(
        _: std.mem.Allocator,
        _: std.Io,
        _: anytype,
        _: ?[]const u8,
        _: []const u8,
    ) error{Disabled}!*DisabledPhuxProvider {
        return error.Disabled;
    }
    pub fn destroy(_: *DisabledPhuxProvider) void {}
    pub fn coordinatorId(_: Endpoint) ProviderId {
        return .phux;
    }
    pub fn providerId(_: *const DisabledPhuxProvider) ProviderId {
        return .phux;
    }
    pub fn effectiveProviderId(_: *const DisabledPhuxProvider) ProviderId {
        return .phux;
    }
    pub fn showing(_: *const DisabledPhuxProvider) bool {
        return false;
    }
    pub fn show(_: *DisabledPhuxProvider, _: u32) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn open(_: *DisabledPhuxProvider, _: native_sdk.ChannelHandle) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn reconnect(_: *DisabledPhuxProvider, _: native_sdk.ChannelHandle) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn stop(_: *DisabledPhuxProvider) void {}
    pub fn state(_: *const DisabledPhuxProvider) State {
        return .new;
    }
    pub fn requestSpawn(_: *DisabledPhuxProvider, _: ?TerminalRef, _: Viewport) error{Disabled}!u32 {
        return error.Disabled;
    }
    pub fn requestAttach(_: *DisabledPhuxProvider, _: TerminalRef) error{Disabled}!u32 {
        return error.Disabled;
    }
    pub fn requestDetach(_: *DisabledPhuxProvider, _: TerminalRef) error{Disabled}!u32 {
        return error.Disabled;
    }
    pub const RenameInfo = struct {
        status: enum(u32) { none = 0, pending = 1, renamed = 2, refused = 3, unknown_outcome = 4, _ } = .none,
        request_id: u32 = 0,
        session_id: u32 = 0,
        sessions_revision: u64 = 0,
        message: []const u8 = "",
    };
    pub fn requestRename(_: *DisabledPhuxProvider, _: []const u8, _: []const u8) error{Disabled}!u32 {
        return error.Disabled;
    }
    pub fn conditionalKillSupported(_: *const DisabledPhuxProvider) bool {
        return false;
    }
    pub fn requestSpawnBound(_: *DisabledPhuxProvider, _: ?TerminalRef, _: Viewport, _: []const u8) error{Disabled}!u32 {
        return error.Disabled;
    }
    pub fn requestKillIf(_: *DisabledPhuxProvider, _: TerminalRef, _: [16]u8) error{Disabled}!u32 {
        return error.Disabled;
    }
    pub fn renameInfo(_: *const DisabledPhuxProvider) @This().RenameInfo {
        return .{};
    }
    pub fn catalogRefs(_: *const DisabledPhuxProvider, _: []TerminalRef) usize {
        return 0;
    }
    pub fn workspaceSnapshot(_: *const DisabledPhuxProvider) provider_contract.workspace.Snapshot {
        return .{};
    }
    pub fn catalogTerminals(_: *const DisabledPhuxProvider) []const provider_contract.workspace.CatalogTerminal {
        return &.{};
    }
    pub fn terminalSession(_: *const DisabledPhuxProvider, _: TerminalRef) ?u32 {
        return null;
    }
    pub fn requestWorkspaceRefresh(_: *DisabledPhuxProvider) error{Disabled}!?u32 {
        return error.Disabled;
    }
    pub fn requestWorkspaceMutation(_: *DisabledPhuxProvider, _: provider_contract.workspace.Mutation) error{Disabled}!u32 {
        return error.Disabled;
    }
    pub fn mouseMode(_: *const DisabledPhuxProvider, _: ReplicaOwner) error{Disabled}!provider_contract.MouseMode {
        return error.Disabled;
    }
    pub fn mouseTracking(_: *const DisabledPhuxProvider, _: ReplicaOwner) error{Disabled}!bool {
        return error.Disabled;
    }
    pub fn sendMouse(_: *DisabledPhuxProvider, _: ReplicaOwner, _: *const MouseInput) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn selectionGesture(_: *DisabledPhuxProvider, _: ReplicaOwner, _: provider_contract.SelectionGesture) error{Disabled}!provider_contract.SelectionGestureResult {
        return error.Disabled;
    }
    pub fn takeOperationResult(_: *DisabledPhuxProvider) ?@This().OperationResult {
        return null;
    }
    pub fn connectionEpoch(_: *const DisabledPhuxProvider) u64 {
        return 0;
    }
    pub fn serverId(_: *const DisabledPhuxProvider) ?[]const u8 {
        return null;
    }
    pub fn remoteLabel(_: *const DisabledPhuxProvider) ?[]const u8 {
        return null;
    }
    pub fn endpointDescriptor(_: *const DisabledPhuxProvider) Endpoint {
        return .{ .unix = "" };
    }
    pub fn drainReadiness(_: *DisabledPhuxProvider) error{Disabled}!@This().SyncDelta {
        return error.Disabled;
    }

    pub fn terminalKnown(_: *const DisabledPhuxProvider, _: TerminalRef) bool {
        return false;
    }
    pub fn sessionCatalog(_: *const DisabledPhuxProvider) []const @This().SessionSummary {
        return &.{};
    }
    pub fn standbyCatalog(_: *const DisabledPhuxProvider) []const @This().SessionSummary {
        return &.{};
    }
    pub fn refreshStandby(_: *DisabledPhuxProvider) void {}
    pub fn agentSessions(_: *const DisabledPhuxProvider) []const @This().AgentSession {
        return &.{};
    }
    pub fn agentSessionsUnder(_: *const DisabledPhuxProvider, _: TerminalRef, _: []*const @This().AgentSession) usize {
        return 0;
    }
    pub fn agentAttention(_: *const DisabledPhuxProvider, _: TerminalRef) bool {
        return false;
    }
    pub fn isAgentSession(_: *const DisabledPhuxProvider, _: TerminalRef) bool {
        return false;
    }
    pub fn selectedSessionId(_: *const DisabledPhuxProvider) ?u32 {
        return null;
    }
    pub fn selectSession(_: *DisabledPhuxProvider, _: u32) error{Disabled}!bool {
        return error.Disabled;
    }
    pub fn terminalRefs(_: *const DisabledPhuxProvider, _: []TerminalRef) usize {
        return 0;
    }
    pub fn contains(_: *const DisabledPhuxProvider, _: TerminalRef) bool {
        return false;
    }
    pub fn owner(_: *const DisabledPhuxProvider, _: TerminalRef) ?ReplicaOwner {
        return null;
    }
    pub fn ownerIsCurrent(_: *const DisabledPhuxProvider, _: ReplicaOwner) bool {
        return false;
    }
    pub fn presentation(_: *const DisabledPhuxProvider, _: TerminalRef) ?Presentation {
        return null;
    }
    pub fn setColorPolicy(_: *const DisabledPhuxProvider, _: anytype) void {}
    pub fn lastViewport(_: *const DisabledPhuxProvider, _: TerminalRef) ?Viewport {
        return null;
    }
    pub fn viewportResize(_: *DisabledPhuxProvider, _: TerminalRef, _: Viewport) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn sendKey(_: *DisabledPhuxProvider, _: ReplicaOwner, _: *const KeyInput) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn sendFocus(_: *DisabledPhuxProvider, _: ReplicaOwner, _: bool) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn sendPaste(_: *DisabledPhuxProvider, _: ReplicaOwner, _: []const u8, _: bool) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn scrollViewport(_: *DisabledPhuxProvider, _: ReplicaOwner, _: Scroll) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn createAnchor(_: *DisabledPhuxProvider, _: ReplicaOwner, _: anytype) error{Disabled}!Anchor {
        return error.Disabled;
    }
    pub fn releaseAnchor(_: *DisabledPhuxProvider, _: ReplicaOwner, _: Anchor) void {}
    pub fn setSelection(_: *DisabledPhuxProvider, _: ReplicaOwner, _: Anchor, _: Anchor, _: bool) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn clearSelection(_: *DisabledPhuxProvider, _: ReplicaOwner) error{Disabled}!void {
        return error.Disabled;
    }
    pub fn selectionText(_: *DisabledPhuxProvider, _: ReplicaOwner, _: std.mem.Allocator) error{Disabled}![]u8 {
        return error.Disabled;
    }
};

pub const PhuxProvider = if (phux_enabled)
    @import("phux_provider").PhuxProvider
else
    DisabledPhuxProvider;
pub const SessionSummary = PhuxProvider.SessionSummary;
pub const OperationResult = PhuxProvider.OperationResult;
pub const RenameInfo = if (phux_enabled) @import("phux_provider").RenameInfo else DisabledPhuxProvider.RenameInfo;
pub const SyncDelta = if (phux_enabled) @import("phux_provider").SyncDelta else DisabledPhuxProvider.SyncDelta;
/// The endpoint a Phux provider dials, in either build.
pub const PhuxEndpoint = if (phux_enabled) @import("phux_provider").Endpoint else DisabledPhuxProvider.Endpoint;
pub const max_remote_sessions: usize = if (phux_enabled) @import("phux_provider").max_sessions else 0;
pub const AgentSession = PhuxProvider.AgentSession;
pub const AgentState = PhuxProvider.AgentState;
/// Roster ceiling, and the size of the row buffer every caller declares.
pub const max_agent_sessions: usize = if (phux_enabled) @import("phux_provider").max_agent_sessions else 0;

const DisabledPointerModule = struct {
    pub const EventQueue = struct {};
    pub const Monitor = struct {};
};
pub const pointer_module = if (phux_enabled) @import("phux_pointer") else DisabledPointerModule;

pub const phux_channel_key: u64 = 102;
pub const pointer_channel_key: u64 = 103;
/// Legacy exported value. Peer routing uses allocated handles, never this key.
pub const phux_peer_channel_key: u64 = 104;
pub const max_remote_terminals: usize = provider_contract.workspace.max_terminals;

/// Allocated effect handles never encode a collection index and never wrap.
var next_peer_handle: std.atomic.Value(u64) = .init(0x5046_0000_0000_0000);

pub fn allocatePeerHandle() error{HandleExhausted}!u64 {
    var current = next_peer_handle.load(.monotonic);
    while (current != std.math.maxInt(u64)) {
        if (next_peer_handle.cmpxchgWeak(current, current + 1, .monotonic, .monotonic)) |actual| {
            current = actual;
        } else return current;
    }
    return error.HandleExhausted;
}

pub const ProviderKind = enum { local, phux };

pub fn providerKind(terminal_ref: TerminalRef) ProviderKind {
    return if (terminal_ref.provider_id == .local) .local else .phux;
}

pub fn localRef(id: LocalResourceId) TerminalRef {
    return provider_contract.localTerminalRef(id);
}

pub fn refEql(a: TerminalRef, b: TerminalRef) bool {
    return a.eql(b);
}

pub fn optRefEql(a: ?TerminalRef, b: ?TerminalRef) bool {
    if (a == null or b == null) return a == null and b == null;
    return a.?.eql(b.?);
}

pub fn optOwnerEql(a: ?ReplicaOwner, b: ?ReplicaOwner) bool {
    if (a == null or b == null) return a == null and b == null;
    return a.?.eql(b.?);
}
