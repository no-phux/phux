//! Identity-first provider around the owning-thread Phux host.

const std = @import("std");
const native_sdk = @import("native_sdk");
const provider = @import("provider_contract");
const host_mod = @import("phux_host");
pub const path_queries = host_mod.path_queries;
const transport = @import("phux_transport");
const extension = @import("phux_extension");
pub const machines = @import("machines.zig");
pub const remote_api = extension.remote;
const captured = @import("captured_registry.zig");
pub const RegistryIdentity = captured.Identity;

test {
    _ = @import("color_policy_tests.zig");
    _ = captured;
    _ = @import("captured_tunnel_tests.zig");
}

pub const enabled = true;
pub const max_sessions = host_mod.max_sessions;
pub const Endpoint = extension.Endpoint;
pub const State = host_mod.State;
pub const Lane = host_mod.Lane;
pub const SyncDelta = host_mod.SyncDelta;
pub const DocumentSpace = host_mod.DocumentSpace;
pub const DocumentPoint = host_mod.DocumentPoint;
pub const Anchor = host_mod.Anchor;
pub const SearchResult = host_mod.SearchResult;
pub const Notice = host_mod.Notice;
pub const SessionSummary = host_mod.SessionSummary;
pub const RenameInfo = host_mod.Host.RenameInfo;
pub const SessionCreateInfo = host_mod.Host.SessionCreateInfo;
pub const Error = host_mod.Error;
pub const logInit = host_mod.logInit;
pub const OperationResult = host_mod.OperationResult;
pub const ColorPolicy = host_mod.ColorPolicy;
pub const max_agent_sessions = host_mod.max_agent_sessions;

const OwnedEndpoint = union(enum) {
    tcp: struct { host: []u8, port: u16 },
    unix: []u8,
    remote: struct { target: []u8, config_path: []u8 },

    fn init(gpa: std.mem.Allocator, endpoint: Endpoint) !OwnedEndpoint {
        return switch (endpoint) {
            .tcp => |tcp| .{ .tcp = .{ .host = try gpa.dupe(u8, tcp.host), .port = tcp.port } },
            .unix => |path| .{ .unix = try gpa.dupe(u8, path) },
            .remote => |remote| blk: {
                const target = try gpa.dupe(u8, remote.target);
                errdefer gpa.free(target);
                break :blk .{ .remote = .{ .target = target, .config_path = try gpa.dupe(u8, remote.config_path) } };
            },
        };
    }
    fn deinit(endpoint: *OwnedEndpoint, gpa: std.mem.Allocator) void {
        switch (endpoint.*) {
            .tcp => |tcp| gpa.free(tcp.host),
            .unix => |path| gpa.free(path),
            .remote => |remote| {
                gpa.free(remote.target);
                gpa.free(remote.config_path);
            },
        }
    }
    fn borrowed(endpoint: *const OwnedEndpoint) Endpoint {
        return switch (endpoint.*) {
            .tcp => |tcp| .{ .tcp = .{ .host = tcp.host, .port = tcp.port } },
            .unix => |path| .{ .unix = path },
            .remote => |remote| .{ .remote = .{ .target = remote.target, .config_path = remote.config_path } },
        };
    }
};

/// What the runtime's wake callback is handed. The callback runs on the
/// driver's thread and may do nothing but post the one-byte readiness token
/// the UI thread already drains for the embedded lane.
const WakeContext = struct {
    handle: ?native_sdk.ChannelHandle = null,
    /// Set before teardown. `stopConnected` joins the driver, so this only
    /// has to cover the degraded path where the placeholder could not be
    /// allocated and the driver outlives the stop.
    stopped: std.atomic.Value(bool) = .init(false),
};

/// The runtime's driver has something to drain. This is the connected lane's
/// `extension.Worker.wake`: same token, same channel, no socket. It runs on
/// the driver's thread and must do nothing else.
fn connectedWake(context: ?*anyopaque) callconv(.c) void {
    const wake: *WakeContext = @ptrCast(@alignCast(context orelse return));
    if (wake.stopped.load(.acquire)) return;
    const handle = wake.handle orelse return;
    _ = handle.post(&transport.wake_payload);
}

/// A host switch waiting for the next connection generation.
const Retarget = struct {
    endpoint: OwnedEndpoint,
    session: ?[]u8,
    label: ?[]u8,
};

/// Numeric session IDs are scoped to one coordinator incarnation. Retain
/// the last authoritative name as reconnect intent, never as create authority.
const SessionIntent = struct {
    id: u32,
    server: []u8,
    name: ?[]u8,

    fn deinit(self: *SessionIntent, gpa: std.mem.Allocator) void {
        gpa.free(self.server);
        if (self.name) |name| gpa.free(name);
    }
};

pub const PhuxProvider = struct {
    pub const test_support = host_mod.test_support;
    pub const SessionSummary = host_mod.SessionSummary;
    pub const OperationResult = host_mod.OperationResult;
    pub const AgentSession = host_mod.AgentSession;
    pub const AgentIdentity = host_mod.AgentIdentity;
    pub const AgentState = host_mod.AgentState;
    context_id: u64,
    gpa: std.mem.Allocator,
    io: std.Io,
    bridge: *transport.Bridge,
    host: *host_mod.Host,
    /// Handed to the runtime's wake callback, which runs on the driver's
    /// thread. It lives as long as the provider so the callback cannot
    /// outlive what it dereferences; `Host.connect`'s client is freed in
    /// `destroy` before this does.
    wake_context: WakeContext = .{},
    /// Cancels a local coordinator ensure on the connected lane, where
    /// there is no worker to carry the worker's own flag.
    connected_stopping: std.atomic.Value(bool) = .init(false),
    endpoint: OwnedEndpoint,
    /// An explicit PHUX_SESSION selects by name. Null means attach the
    /// server's current session. Neither path has create authority.
    session: ?[]u8,
    session_id: ?u32 = null,
    session_intent: ?SessionIntent = null,
    client_name: []u8,
    attach_viewport: provider.Viewport = .{ .cols = 80, .rows = 24 },
    attach_queued: bool = false,
    /// Failure record shared with each socket worker of a remote endpoint.
    remote_status: extension.remote.Status = .{},
    local_status: extension.startup.Status = .{},
    /// What the catalog and status line call a remote endpoint: the registry
    /// entry's name when Connect to Host resolved one, else the target.
    remote_label: ?[]u8 = null,
    /// Connect to Host, applied by the next `open`/`reconnect` after the old
    /// worker has stopped; see `requestRetarget`.
    pending_retarget: ?Retarget = null,
    /// A Machines-selected destination. Its opaque tunnel retains the checked
    /// endpoint/pin/token provenance; only this credential-free identity is UI-facing.
    capture: ?captured.Capture = null,
    /// The standby coordinator of a side-by-side switcher (`standBy`). It
    /// never attaches: attaching would make it a subscriber that clamps
    /// every pane under `window-size = smallest` to its placeholder size and
    /// streams output nobody sees. It only lists sessions with GET_STATE.
    standby: bool = false,
    /// The connection epoch whose session list the standby asked for.
    standby_query_epoch: u64 = 0,

    pub fn create(gpa: std.mem.Allocator, io: std.Io, endpoint: Endpoint, session: ?[]const u8, client_name: []const u8) !*PhuxProvider {
        const self = try gpa.create(PhuxProvider);
        errdefer gpa.destroy(self);
        const bridge = try gpa.create(transport.Bridge);
        errdefer gpa.destroy(bridge);
        bridge.* = transport.Bridge.init(gpa);
        errdefer bridge.deinit();
        const host = try host_mod.Host.create(gpa, bridge);
        errdefer host.destroy();
        var owned_endpoint = try OwnedEndpoint.init(gpa, endpoint);
        errdefer owned_endpoint.deinit(gpa);
        const owned_session = if (session) |name| try gpa.dupe(u8, name) else null;
        errdefer if (owned_session) |name| gpa.free(name);
        const owned_client_name = try gpa.dupe(u8, client_name);
        errdefer gpa.free(owned_client_name);
        const owned_label: ?[]u8 = switch (endpoint) {
            .remote => |remote| try gpa.dupe(u8, remote.target),
            else => null,
        };
        errdefer if (owned_label) |label| gpa.free(label);
        self.* = .{ .gpa = gpa, .io = io, .bridge = bridge, .host = host, .endpoint = owned_endpoint, .session = owned_session, .client_name = owned_client_name, .remote_label = owned_label, .context_id = try provider.context.allocate() };
        host.setProviderId(coordinatorId(endpoint));
        return self;
    }

    /// Consumes the exact checked tunnel on every return. Registry identity is
    /// copied before the callback's borrowed row can be refreshed or released.
    pub fn createCaptured(gpa: std.mem.Allocator, io: std.Io, tunnel: extension.remote.Tunnel, identity: RegistryIdentity, client_name: []const u8) !*PhuxProvider {
        var capture = try captured.Capture.init(gpa, tunnel, identity);
        errdefer capture.deinit(gpa);
        const session: ?[]const u8 = if (identity.session.len == 0) null else identity.session;
        const self = try create(gpa, io, .{ .remote = .{ .target = identity.name } }, session, client_name);
        self.capture = capture;
        self.host.setProviderId(provider.phuxCoordinatorId(identity.endpoint));
        return self;
    }

    pub fn registryIdentity(self: *const PhuxProvider) ?RegistryIdentity {
        if (self.pending_retarget != null) return null;
        const capture = self.capture orelse return null;
        return capture.identity;
    }

    /// An independent client/worker for another visible session at this exact
    /// captured destination. Only immutable dial config is copied. The caller
    /// selects the session on the new provider before opening it.
    pub fn createSiblingAttachment(self: *const PhuxProvider, gpa: std.mem.Allocator, io: std.Io, client_name: []const u8) !*PhuxProvider {
        if (self.pending_retarget != null) return error.InvalidState;
        if (self.endpoint != .remote) return create(gpa, io, self.endpointDescriptor(), null, client_name);
        const identity = self.registryIdentity() orelse return error.InvalidRegistryIdentity;
        const tunnel = try self.capture.?.cloneTunnel();
        return createCaptured(gpa, io, tunnel, identity, client_name);
    }

    /// Machines Retry supplies a fresh registry-validated tunnel for the same
    /// exact identity, then invokes reconnect. Consumes even when rejected.
    pub fn replaceCapturedTunnel(self: *PhuxProvider, tunnel: extension.remote.Tunnel, identity: RegistryIdentity) !void {
        if (self.pending_retarget != null) {
            tunnel.close();
            return error.InvalidRegistryIdentity;
        }
        const capture = if (self.capture) |*value| value else {
            tunnel.close();
            return error.InvalidRegistryIdentity;
        };
        try capture.replace(tunnel, identity);
    }

    fn clearCapture(self: *PhuxProvider) void {
        if (self.capture) |*capture| capture.deinit(self.gpa);
        self.capture = null;
    }

    fn cancelCapturedTunnel(self: *PhuxProvider) void {
        if (self.capture) |*capture| capture.cancel();
    }

    /// The coordinator an endpoint reaches (contract.phuxCoordinatorId).
    pub fn coordinatorId(endpoint: Endpoint) provider.ProviderId {
        return provider.phuxCoordinatorId(switch (endpoint) {
            .remote => |remote| remote.target,
            else => null,
        });
    }

    /// The coordinator this provider is connected to, which every ref it
    /// publishes carries. It follows the applied endpoint: a pending
    /// retarget moves it only when the next connection starts.
    pub fn providerId(self: *const PhuxProvider) provider.ProviderId {
        return self.host.provider_id;
    }

    /// The coordinator this provider dials next, pending retarget included.
    pub fn effectiveProviderId(self: *const PhuxProvider) provider.ProviderId {
        if (self.pending_retarget == null) return self.providerId();
        return coordinatorId(self.effectiveEndpoint().borrowed());
    }

    pub fn destroy(self: *PhuxProvider) void {
        self.stop();
        self.host.destroy();
        self.bridge.deinit();
        self.gpa.destroy(self.bridge);
        self.endpoint.deinit(self.gpa);
        self.clearCapture();
        self.clearPendingRetarget();
        if (self.remote_label) |label| self.gpa.free(label);
        if (self.session) |session| self.gpa.free(session);
        self.clearSessionIntent();
        self.gpa.free(self.client_name);
        self.gpa.destroy(self);
    }

    /// Hand the target to the runtime, which dials, walks the reconnect
    /// ladder and wakes this channel itself (ADR-0133).
    pub fn open(self: *PhuxProvider, handle: native_sdk.ChannelHandle) !void {
        errdefer self.cancelCapturedTunnel();
        if (self.host.lane == .connected) return error.InvalidState;
        self.applyPendingRetarget();
        self.wake_context.handle = handle;
        self.wake_context.stopped.store(false, .release);
        if (self.capture) |*capture| {
            // The capture is the destination: its retained endpoint, pin and
            // token provenance outrank the registry alias, which may since
            // have been retargeted. The runtime only reads that
            // configuration, so this tunnel is ours to free.
            var tunnel = try capture.take();
            defer tunnel.close();
            return self.host.connectCaptured(
                tunnel.handle,
                self.client_name,
                connectedWake,
                &self.wake_context,
            );
        }
        const target = try self.connectTarget();
        // The runtime dials; it does not start anything. A local coordinator
        // is still this machine's to supervise, exactly as the worker
        // supervised it, or the ladder would spin against a socket nobody
        // is listening on.
        try self.ensureLocalCoordinator();
        try self.host.connect(target, self.client_name, connectedWake, &self.wake_context);
    }

    /// `extension.Worker.ensureLocal`, for the lane that has no worker.
    fn ensureLocalCoordinator(self: *PhuxProvider) !void {
        const path = switch (self.endpoint.borrowed()) {
            .unix => |path| path,
            // A remote coordinator is the remote host's to supervise.
            .tcp, .remote => return,
        };
        self.connected_stopping.store(false, .release);
        var evidence: extension.startup.Evidence = .{};
        const options: extension.startup.Options = .{
            .evidence = &evidence,
            .status = &self.local_status,
        };
        extension.startup.ensure(self.gpa, self.io, path, &self.connected_stopping, options) catch |err| {
            self.local_status.record(evidence, path, err);
            return err;
        };
        self.local_status.record(evidence, path, null);
    }

    /// The runtime dials a local socket or a registered host. A bare
    /// `host:port` has no runtime lane -- the registry is what carries the
    /// pin and token a routable dial needs -- so it stays the embedded
    /// lane's, where the Zig worker owns the connect.
    fn connectTarget(self: *const PhuxProvider) !host_mod.ConnectTarget {
        return switch (self.endpoint.borrowed()) {
            .unix => |path| .{ .socket_path = path },
            .remote => |remote| .{ .target = remote.target, .config_path = remote.config_path },
            .tcp => error.Unsupported,
        };
    }

    pub fn stop(self: *PhuxProvider) void {
        self.captureAttachedSession() catch {};
        self.cancelCapturedTunnel();
        // Freeing the connected client joins the runtime's driver, so no
        // wake can reach `wake_context` after this returns.
        self.connected_stopping.store(true, .release);
        self.wake_context.stopped.store(true, .release);
        self.host.stopConnected();
        self.host.disconnect();
        self.attach_queued = false;
        self.standby_query_epoch = 0;
        // A standby's list described the connection that just ended.
        if (self.standby) self.host.forgetSessions();
    }

    /// Preserve provider identity, terminal order, and the last complete canvas
    /// while replacing only the generation-bound client and socket worker.
    pub fn reconnect(self: *PhuxProvider, handle: native_sdk.ChannelHandle) !void {
        try self.restartConnection(handle);
    }

    pub fn selectSession(self: *PhuxProvider, session_id: u32) !bool {
        if (session_id == 0) return error.InvalidIdentity;
        var found = false;
        for (self.host.sessionCatalog()) |session| {
            if (session.id == session_id) {
                try self.rememberCatalogSession(session_id, session.name);
                found = true;
                break;
            }
        }
        if (!found) return error.InvalidIdentity;
        // A pending destination takes precedence over the last attachment: an
        // A -> B -> A intent must cancel B even while A is still displayed.
        const requested: ?u32 = self.session_id orelse self.selectedSessionId();
        if (requested == session_id) return false;
        self.session_id = session_id;
        return true;
    }

    /// The runtime already owns redialing, so an unchanged destination only
    /// asks it to redial now. A retarget is a different destination, which
    /// needs a new session: tear the old one down and connect again.
    fn restartConnection(self: *PhuxProvider, handle: native_sdk.ChannelHandle) !void {
        errdefer self.cancelCapturedTunnel();
        try self.captureAttachedSession();
        // Never opened, or already torn down: there is no connection to
        // retire, so this is an open. The previous generation still freezes,
        // because a refused open must leave its canvas rather than drop it.
        if (self.host.lane != .connected) {
            self.host.freezePublished();
            return self.open(handle);
        }
        if (self.pending_retarget == null) {
            // Retire now, so an explicit reconnect presents the same way it
            // does on the embedded lane, then let the runtime redial.
            try self.host.reconnectConnected();
            self.attach_queued = false;
            return;
        }
        self.host.freezePublished();
        errdefer self.host.freezePublished();
        self.wake_context.stopped.store(true, .release);
        self.host.stopConnected();
        self.attach_queued = false;
        try self.open(handle);
    }

    /// Point the next connection at a different coordinator: a registered
    /// remote host, or back to the local socket. Nothing changes until the
    /// next `open`/`reconnect` starts a worker, so moving between hosts rides
    /// the engine's ordinary restart path (frozen canvases, session handoff,
    /// close-before-reopen) instead of a second lifecycle.
    pub fn requestRetarget(self: *PhuxProvider, endpoint: Endpoint, session: ?[]const u8, label: ?[]const u8) !void {
        self.commitRetarget(try self.prepareRetarget(endpoint, session, label));
    }

    /// The allocating half of `requestRetarget`, so a caller moving two
    /// providers can fail before either one changes.
    pub fn prepareRetarget(self: *const PhuxProvider, endpoint: Endpoint, session: ?[]const u8, label: ?[]const u8) !Retarget {
        var next_endpoint = try OwnedEndpoint.init(self.gpa, endpoint);
        errdefer next_endpoint.deinit(self.gpa);
        const next_session = if (session) |name| try self.gpa.dupe(u8, name) else null;
        errdefer if (next_session) |name| self.gpa.free(name);
        const next_label = if (label) |text| try self.gpa.dupe(u8, text) else null;
        return .{ .endpoint = next_endpoint, .session = next_session, .label = next_label };
    }

    /// The infallible half. A standby forgets its list at once: it described
    /// the host it is leaving, and must not be offered under the new label.
    pub fn commitRetarget(self: *PhuxProvider, next: Retarget) void {
        self.clearPendingRetarget();
        self.pending_retarget = next;
        // The new host starts with no history, even before it is applied.
        self.remote_status.reset();
        if (self.standby) self.host.forgetSessions();
    }

    pub fn discardRetarget(self: *const PhuxProvider, next: Retarget) void {
        var endpoint = next.endpoint;
        endpoint.deinit(self.gpa);
        if (next.session) |name| self.gpa.free(name);
        if (next.label) |text| self.gpa.free(text);
    }

    /// Make this provider the standby coordinator: it lists sessions with
    /// GET_STATE and never attaches (see `standby`).
    pub fn standBy(self: *PhuxProvider) void {
        self.standby = true;
        self.clearSessionIntent();
        self.session_id = null;
    }

    /// Whether this coordinator's terminals may be on screen: it attaches a
    /// session. A standby only lists.
    pub fn showing(self: *const PhuxProvider) bool {
        return !self.standby;
    }

    /// Show `session_id` of this listing coordinator beside the others. The
    /// next connection attaches it the way the active provider attaches its
    /// own; each terminal then gets its real size from the sizing pump once
    /// a pane shows it. Takes effect when the connection restarts.
    pub fn show(self: *PhuxProvider, session_id: u32) !void {
        if (session_id == 0) return error.InvalidIdentity;
        for (self.host.sessionCatalog()) |entry| {
            if (entry.id == session_id) {
                try self.rememberCatalogSession(session_id, entry.name);
                break;
            }
        }
        self.standby = false;
        self.session_id = session_id;
        self.attach_queued = false;
        self.standby_query_epoch = 0;
    }

    /// Once per connection, after negotiation.
    fn queueStandbyQuery(self: *PhuxProvider) !void {
        if (self.host.state() != .negotiated) return;
        if (self.standby_query_epoch == self.connectionEpoch()) return;
        _ = try self.host.querySessions();
        self.standby_query_epoch = self.connectionEpoch();
    }

    /// Ask the standby's server again, as a switcher refresh does for the
    /// active coordinator; at most one query is outstanding.
    pub fn refreshStandby(self: *PhuxProvider) void {
        if (!self.standby or self.host.state() != .negotiated) return;
        _ = self.host.querySessions() catch {};
    }

    /// The standby's sessions while they describe the host it is connected
    /// to: negotiated (or attached), listed on this connection, and with no
    /// retarget pending. Anything else lists nothing.
    pub fn standbyCatalog(self: *const PhuxProvider) []const host_mod.SessionSummary {
        if (self.pending_retarget != null) return &.{};
        const current = self.host.state();
        if (current != .negotiated and current != .attached) return &.{};
        if (self.host.sessions_generation != self.host.connectionEpoch()) return &.{};
        return self.host.sessionCatalog();
    }

    fn clearPendingRetarget(self: *PhuxProvider) void {
        const pending = self.pending_retarget orelse return;
        var endpoint = pending.endpoint;
        endpoint.deinit(self.gpa);
        if (pending.session) |name| self.gpa.free(name);
        if (pending.label) |text| self.gpa.free(text);
        self.pending_retarget = null;
    }

    /// Only with no worker running. Session IDs, replicas and the catalog
    /// belong to the old coordinator, exactly as for an explicit session
    /// switch, so they are released before the new host can publish.
    fn applyPendingRetarget(self: *PhuxProvider) void {
        const next = self.pending_retarget orelse return;
        self.clearCapture();
        self.pending_retarget = null;
        self.endpoint.deinit(self.gpa);
        self.endpoint = next.endpoint;
        if (self.session) |name| self.gpa.free(name);
        self.session = next.session;
        if (self.remote_label) |text| self.gpa.free(text);
        self.remote_label = next.label;
        self.session_id = null;
        self.clearSessionIntent();
        if (self.host.state() != .new) self.host.clearSessionReplicas();
        // Replicas are gone, so no terminal keeps the old coordinator's id.
        self.host.setProviderId(coordinatorId(self.endpoint.borrowed()));
        self.remote_status.reset();
    }

    fn workerEndpoint(self: *PhuxProvider) Endpoint {
        var endpoint = self.endpoint.borrowed();
        if (endpoint == .remote) endpoint.remote.status = &self.remote_status;
        return endpoint;
    }

    fn effectiveEndpoint(self: *const PhuxProvider) *const OwnedEndpoint {
        if (self.pending_retarget) |*pending| return &pending.endpoint;
        return &self.endpoint;
    }

    /// The registered host this provider dials or is about to dial; null for
    /// a local coordinator.
    pub fn remoteTarget(self: *const PhuxProvider) ?[]const u8 {
        return switch (self.effectiveEndpoint().*) {
            .remote => |remote| remote.target,
            else => null,
        };
    }

    /// The name the catalog and status line give that host.
    pub fn remoteLabel(self: *const PhuxProvider) ?[]const u8 {
        const target = self.remoteTarget() orelse return null;
        if (self.pending_retarget) |pending| return pending.label orelse target;
        return self.remote_label orelse target;
    }

    /// Name the current remote endpoint by its registry entry, as Connect to
    /// Host does through `requestRetarget`. Used when a host selected at
    /// launch has been resolved before the provider's first connection.
    pub fn setRemoteLabel(self: *PhuxProvider, label: []const u8) !void {
        const owned = try self.gpa.dupe(u8, label);
        if (self.remote_label) |previous| self.gpa.free(previous);
        self.remote_label = owned;
    }

    /// An owned copy of where this provider's next connection goes: the
    /// pending retarget when there is one, else the applied endpoint, with
    /// its session and label. A host exchange copies each side before either
    /// retarget, because a retarget frees the pending target that a borrowed
    /// slice would point at.
    pub const TargetCopy = struct {
        endpoint: OwnedEndpoint,
        session: ?[]u8,
        label: ?[]u8,

        pub fn descriptor(self: *const TargetCopy) Endpoint {
            return self.endpoint.borrowed();
        }

        pub fn deinit(self: *TargetCopy, gpa: std.mem.Allocator) void {
            self.endpoint.deinit(gpa);
            if (self.session) |value| gpa.free(value);
            if (self.label) |value| gpa.free(value);
        }
    };

    pub fn copyTarget(self: *const PhuxProvider, gpa: std.mem.Allocator) !TargetCopy {
        // An alias-only copy cannot carry the checked pin/token provenance.
        if (self.registryIdentity() != null) return error.CapturedTunnelRequired;
        var endpoint = try OwnedEndpoint.init(gpa, self.effectiveEndpoint().borrowed());
        errdefer endpoint.deinit(gpa);
        const source: ?[]const u8 = if (self.pending_retarget) |pending| pending.session else self.currentSessionName();
        const session = if (source) |value| try gpa.dupe(u8, value) else null;
        errdefer if (session) |value| gpa.free(value);
        const label = if (self.remoteLabel()) |value| try gpa.dupe(u8, value) else null;
        return .{ .endpoint = endpoint, .session = session, .label = label };
    }

    /// The session a host exchange returns to: the attached one by name,
    /// else the one this connection was asked for. Borrowed from the host's
    /// session catalog or the applied session, until the next retarget applies.
    pub fn currentSessionName(self: *const PhuxProvider) ?[]const u8 {
        if (self.host.selectedSessionId()) |attached| {
            for (self.host.sessionCatalog()) |entry| {
                if (entry.id == attached and entry.name.len != 0) return entry.name;
            }
        }
        return self.session;
    }

    /// The last recorded connection failure, copied into `out`.
    /// Carry the runtime's reason into the record the Machines UI reads.
    ///
    /// The socket worker used to copy this off the relay tunnel it owned.
    /// The runtime owns the dial now, so its last error is the reason, and
    /// without this the UI would show a failed host with nothing to say.
    fn recordRemoteFailure(self: *PhuxProvider) void {
        if (self.endpoint != .remote) return;
        if (self.host.state() == .attached) {
            self.remote_status.noteConnected();
            return;
        }
        // Whenever the runtime has a reason, that reason is the record. The
        // state is not the gate: a host that retired the connection may have
        // moved on from `failed` by the time a drain gets here, and a UI
        // showing a failed host with nothing to say is the bug.
        var buffer: [extension.remote.max_text_bytes]u8 = undefined;
        const message = self.host.copyConnectionError(&buffer);
        if (message.len != 0) self.remote_status.recordFailure(message);
    }

    pub fn remoteFailure(self: *const PhuxProvider, out: []u8) []const u8 {
        if (self.endpoint == .unix) return self.local_status.failureInto(out);
        return self.remote_status.failureInto(out);
    }

    pub fn localToolCli(self: *const PhuxProvider, out: []u8) ?[]const u8 {
        if (self.endpoint != .unix) return null;
        return self.local_status.cliInto(out);
    }

    /// Whether this host has connected since it was selected, which is what
    /// separates "connecting" from "reconnecting".
    pub fn remoteConnectedOnce(self: *const PhuxProvider) bool {
        return self.remote_status.connectedOnce();
    }

    pub fn noteRemoteConnected(self: *PhuxProvider) void {
        self.remote_status.noteConnected();
    }

    pub const RemoteDescription = extension.remote.Description;

    /// Resolve a host in the phux CLI's registry without dialing: immediate
    /// feedback for Connect to Host on the UI thread.
    pub fn describeRemote(target: []const u8) RemoteDescription {
        return extension.remote.describe(target, "");
    }

    /// Explicit session switches release old replica slots before incoming
    /// effects can admit a full replacement inventory. Reconnects do not.
    pub fn prepareSessionSwitch(self: *PhuxProvider) void {
        const target = self.session_id orelse return;
        const attached = self.host.selectedSessionId() orelse return;
        if (target != attached) self.host.clearSessionReplicas();
    }

    /// ATTACH is queued once after negotiation. The host still withholds every
    /// first presentation until the ATTACHED/READY barrier completes.
    pub fn drainReadiness(self: *PhuxProvider) !SyncDelta {
        return self.drainReadinessBudget(self.bridge.incoming.pendingCount());
    }

    /// Whether a drain would find anything. The connected lane's frames are
    /// the runtime's, not the bridge's, so a caller polling providers in
    /// turn must ask through here rather than reading `bridge.incoming`.
    pub fn wakePending(self: *const PhuxProvider) bool {
        return self.host.hasReadiness();
    }

    pub fn drainReadinessBudget(self: *PhuxProvider, frame_limit: usize) !SyncDelta {
        const delta = self.host.drainReadinessBudget(frame_limit) catch |err| {
            self.recordRemoteFailure();
            return err;
        };
        self.recordRemoteFailure();
        // Poll may have consumed the wake carrying a new epoch. Schedule one
        // more UI drain of its retained records; never redial from this layer.
        if (self.host.poll_unsettled) connectedWake(&self.wake_context);
        // Handle replacement and runtime redial share one retirement signal.
        if (self.host.takeConnectionRetired()) {
            self.attach_queued = false;
            self.standby_query_epoch = 0;
        }
        if (self.host.state() == .detached) {
            self.attach_queued = false;
            return delta;
        }
        // A standby lists, it never attaches (see `standby`).
        if (self.standby) {
            try self.queueStandbyQuery();
            return delta;
        }
        try self.queueNegotiatedAttach();
        if (delta.ready_published) self.session_id = self.host.selectedSessionId();
        try self.captureAttachedSession();
        return delta;
    }

    fn queueNegotiatedAttach(self: *PhuxProvider) !void {
        if (self.attach_queued or self.host.state() != .negotiated) return;
        if (self.session_id) |session_id| {
            try self.attachSelectedSession(session_id);
        } else {
            try self.host.attachExisting(self.session, self.attach_viewport);
        }
        self.attach_queued = true;
    }

    fn attachSelectedSession(self: *PhuxProvider, id: u32) !void {
        const server = self.serverId() orelse return error.InvalidIdentity;
        const intent = self.session_intent orelse {
            // A first explicit selection (including a new showing peer) has
            // no previous incarnation. Qualify it before the first ATTACH.
            // A failed copy of an old attachment's identity is not permission
            // to treat its retained number as a first selection.
            if (self.host.selectedSessionId() != null) return error.InvalidIdentity;
            try self.rememberSession(id, null);
            return self.host.attachSessionId(id, self.attach_viewport);
        };
        if (intent.id != id) return error.InvalidIdentity;
        if (std.mem.eql(u8, intent.server, server))
            return self.host.attachSessionId(id, self.attach_viewport);
        // Never send a recycled ID to another coordinator, or fall back to
        // LAST/the configured startup session when the intended name is gone.
        self.host.freezePublished();
        const name = intent.name orelse return error.InvalidIdentity;
        try self.host.attachExisting(name, self.attach_viewport);
    }

    fn captureAttachedSession(self: *PhuxProvider) !void {
        if (self.standby or self.host.state() != .attached) return;
        const id = self.host.selectedSessionId() orelse return;
        if (self.session_id != null and self.session_id != id) return;
        self.session_id = id;
        var name: ?[]const u8 = null;
        if (self.host.sessions_generation == self.connectionEpoch()) {
            for (self.host.sessionCatalog()) |entry| {
                if (entry.id == id) {
                    name = entry.name;
                    break;
                }
            }
        }
        try self.rememberSession(id, name);
    }

    fn rememberCatalogSession(self: *PhuxProvider, id: u32, name: []const u8) !void {
        if (self.host.sessions_generation != self.connectionEpoch()) {
            // Retained rows may still identify a pending same-server switch,
            // but HELLO from a replacement cannot requalify their old IDs.
            const intent = self.session_intent orelse return error.InvalidIdentity;
            const server = self.serverId() orelse return error.InvalidIdentity;
            if (!std.mem.eql(u8, intent.server, server)) return error.InvalidIdentity;
        }
        try self.rememberSession(id, name);
    }

    fn rememberSession(self: *PhuxProvider, id: u32, catalog_name: ?[]const u8) !void {
        const server = self.serverId() orelse return;
        var name = catalog_name;
        if (self.session_intent) |intent| {
            if (intent.id == id and std.mem.eql(u8, intent.server, server)) {
                if (name == null) name = intent.name;
                if (std.mem.eql(u8, name orelse "", intent.name orelse "")) return;
            }
        }
        const owned_server = try self.gpa.dupe(u8, server);
        errdefer self.gpa.free(owned_server);
        const owned_name = if (name) |value| try self.gpa.dupe(u8, value) else null;
        self.clearSessionIntent();
        self.session_intent = .{ .id = id, .server = owned_server, .name = owned_name };
    }

    fn clearSessionIntent(self: *PhuxProvider) void {
        if (self.session_intent) |*intent| intent.deinit(self.gpa);
        self.session_intent = null;
    }

    pub fn state(self: *const PhuxProvider) State {
        return self.host.state();
    }

    pub fn requestSpawn(self: *PhuxProvider, owner_ref: ?provider.TerminalRef, viewport: provider.Viewport) !u32 {
        return self.host.requestSpawn(owner_ref, viewport);
    }
    /// A spawn whose shell starts in `cwd` on the serving host.
    pub fn requestSpawnIn(self: *PhuxProvider, owner_ref: ?provider.TerminalRef, viewport: provider.Viewport, cwd: []const u8) !u32 {
        return self.host.requestSpawnIn(owner_ref, viewport, cwd);
    }
    pub fn requestAttach(self: *PhuxProvider, terminal_ref: provider.TerminalRef) !u32 {
        return self.host.requestAttach(terminal_ref);
    }

    pub const DirectoryInfo = host_mod.Host.DirectoryInfo;
    pub const DirectoryEntry = host_mod.Host.DirectoryEntry;

    /// Go to Directory (docs/spec/L3.md section 4): whether the connected
    /// server lists directories, one request at a time, and its retained
    /// answer. Whichever coordinator this provider dials answers, so a
    /// registered remote host lists its own filesystem.
    pub fn directorySupported(self: *const PhuxProvider) bool {
        return self.host.directoryInfo().supported;
    }
    pub fn requestDirectory(self: *PhuxProvider, path: []const u8) !u32 {
        return self.host.requestDirectory(path);
    }
    /// A satellite of the attached hub lists `path` (docs/spec/L3.md
    /// section 4.1); refused unless `directoryHostSupported`.
    pub fn requestDirectoryOn(self: *PhuxProvider, path: []const u8, satellite: []const u8) !u32 {
        return self.host.requestDirectoryOn(path, satellite);
    }
    pub fn directoryHostSupported(self: *const PhuxProvider) bool {
        return self.host.directoryHostSupported();
    }
    /// Borrowed until the next mutable provider call.
    pub fn directoryInfo(self: *const PhuxProvider) DirectoryInfo {
        return self.host.directoryInfo();
    }
    /// Borrowed until the next mutable provider call.
    pub fn directoryEntry(self: *const PhuxProvider, index: usize) ?DirectoryEntry {
        return self.host.directoryEntry(index);
    }

    /// Rename a session of this coordinator, on this coordinator's connection
    /// alone (host.requestRename). Refused without a live connection.
    pub fn requestRename(self: *PhuxProvider, current: []const u8, new_name: []const u8) !u32 {
        return self.host.requestRename(current, new_name);
    }

    pub fn renameInfo(self: *const PhuxProvider) RenameInfo {
        return self.host.renameInfo();
    }

    pub fn requestCreateSession(self: *PhuxProvider, name: []const u8, keep_empty: bool) !u32 {
        return self.host.requestCreateSession(name, keep_empty);
    }

    pub fn sessionCreateInfo(self: *PhuxProvider, request_id: u32) SessionCreateInfo {
        return self.host.sessionCreateInfo(request_id);
    }

    pub fn releaseSessionCreate(self: *PhuxProvider, request_id: u32) void {
        self.host.releaseSessionCreate(request_id);
    }

    /// CONDITIONAL_KILL on this coordinator's current connection (ADR-0109).
    pub fn conditionalKillSupported(self: *const PhuxProvider) bool {
        return self.host.conditionalKillSupported();
    }

    pub fn requestSpawnBound(self: *PhuxProvider, owner_ref: ?provider.TerminalRef, viewport: provider.Viewport, cwd: []const u8) !u32 {
        return self.host.requestSpawnBound(owner_ref, viewport, cwd);
    }

    /// Dedicated LOCAL tool creation never inherits the focused remote owner.
    pub fn requestSpawnArgvBound(self: *PhuxProvider, owner_ref: ?provider.TerminalRef, viewport: provider.Viewport, cwd: []const u8, argv: []const []const u8) !u32 {
        if (self.endpoint != .unix) return error.InvalidState;
        return self.host.requestSpawnArgvBound(owner_ref, viewport, cwd, argv);
    }

    /// A conditional kill of this coordinator's own bound spawn, on this
    /// coordinator's connection alone (host.requestKillIf).
    pub fn requestKillIf(self: *PhuxProvider, terminal_ref: provider.TerminalRef, instance: [16]u8) !u32 {
        return self.host.requestKillIf(terminal_ref, instance);
    }

    /// Close only on the connection epoch captured with the invoking owner.
    pub fn requestCloseResource(self: *PhuxProvider, terminal_ref: provider.TerminalRef, expected_epoch: u64) !u32 {
        return self.host.requestCloseResource(terminal_ref, expected_epoch);
    }

    pub fn requestCloseResources(self: *PhuxProvider, refs: []const provider.TerminalRef, expected_epoch: u64) !u32 {
        return self.host.requestCloseResources(refs, expected_epoch);
    }

    /// Owned by the caller's buffer; copy immediately after a synchronous refusal.
    pub fn copyLastError(self: *const PhuxProvider, out: []u8) []const u8 {
        return self.host.copyLastError(out);
    }

    pub fn requestDetach(self: *PhuxProvider, terminal_ref: provider.TerminalRef) !u32 {
        return self.host.requestDetach(terminal_ref);
    }

    pub fn catalogRefs(self: *const PhuxProvider, out: []provider.TerminalRef) usize {
        return self.host.catalogRefs(out);
    }
    pub fn workspaceSnapshot(self: *const PhuxProvider) provider.workspace.Snapshot {
        return self.host.workspaceSnapshot();
    }
    pub fn catalogTerminals(self: *const PhuxProvider) []const provider.workspace.CatalogTerminal {
        return self.host.catalogTerminals();
    }
    pub fn terminalSession(self: *const PhuxProvider, ref: provider.TerminalRef) ?u32 {
        return self.host.terminalSession(ref);
    }
    pub fn requestWorkspaceRefresh(self: *PhuxProvider) !?u32 {
        return self.host.requestWorkspaceRefresh();
    }
    pub fn requestWorkspaceMutation(self: *PhuxProvider, value: provider.workspace.Mutation) !u32 {
        return self.host.requestWorkspaceMutation(value);
    }
    pub fn takeOperationResult(self: *PhuxProvider) ?host_mod.OperationResult {
        return self.host.takeOperationResult();
    }
    pub fn connectionEpoch(self: *const PhuxProvider) u64 {
        return self.host.connectionEpoch();
    }
    /// Opaque borrowed bytes; copy before a mutable provider call.
    pub fn serverId(self: *const PhuxProvider) ?[]const u8 {
        return self.host.serverId();
    }
    /// The canonical descriptor used by the socket worker, borrowed until destroy.
    pub fn endpointDescriptor(self: *const PhuxProvider) Endpoint {
        return self.endpoint.borrowed();
    }

    pub fn terminalRefs(self: *const PhuxProvider, out: []provider.TerminalRef) usize {
        return self.host.terminalRefs(out);
    }
    /// Agent sessions from the resource catalog, in catalog order.
    pub fn agentSessions(self: *const PhuxProvider) []const AgentSession {
        return self.host.agentSessions();
    }

    /// Agent identities declared on Terminal resources, without an
    /// AgentSession resource or record stream.
    pub fn agentIdentities(self: *const PhuxProvider) []const AgentIdentity {
        return self.host.agentIdentities();
    }

    /// The agent sessions running under one terminal.
    pub fn agentSessionsUnder(self: *const PhuxProvider, terminal_ref: provider.TerminalRef, out: []*const AgentSession) usize {
        return self.host.agentSessionsUnder(terminal_ref, out);
    }

    /// Stream-derived attention for one terminal.
    pub fn agentAttention(self: *const PhuxProvider, terminal_ref: provider.TerminalRef) bool {
        return self.host.agentAttention(terminal_ref);
    }

    /// Whether this identity names an agent session. Nothing that renders a
    /// terminal surface may be reached through one.
    pub fn isAgentSession(self: *const PhuxProvider, terminal_ref: provider.TerminalRef) bool {
        return self.host.isAgentSession(terminal_ref);
    }

    pub fn sessionCatalog(self: *const PhuxProvider) []const host_mod.SessionSummary {
        return self.host.sessionCatalog();
    }
    pub fn selectedSessionId(self: *const PhuxProvider) ?u32 {
        return self.host.selectedSessionId();
    }
    pub fn contains(self: *const PhuxProvider, terminal_ref: provider.TerminalRef) bool {
        return self.host.contains(terminal_ref);
    }
    pub fn owner(self: *const PhuxProvider, terminal_ref: provider.TerminalRef) ?provider.ReplicaOwner {
        return self.host.owner(terminal_ref);
    }
    pub fn ownerIsCurrent(self: *const PhuxProvider, value: provider.ReplicaOwner) bool {
        return self.host.ownerIsCurrent(value);
    }
    pub fn presentation(self: *const PhuxProvider, terminal_ref: provider.TerminalRef) ?provider.Presentation {
        return self.host.presentation(terminal_ref);
    }

    pub const FrozenPresentation = host_mod.FrozenPresentation;

    pub fn capturePresentation(self: *const PhuxProvider, expected: provider.ReplicaOwner) !*FrozenPresentation {
        return self.host.capturePresentation(expected);
    }

    pub fn terminalKnown(self: *const PhuxProvider, ref: provider.TerminalRef) bool {
        return self.host.terminalKnown(ref);
    }

    /// Logical constness matches local Session.snapshot: update the owned
    /// paint cache, without changing provider identity or engine state.
    pub fn setColorPolicy(self: *const PhuxProvider, policy: ColorPolicy) void {
        self.host.setColorPolicy(policy);
    }
    pub fn lastViewport(self: *const PhuxProvider, terminal_ref: provider.TerminalRef) ?provider.Viewport {
        return self.host.lastViewport(terminal_ref);
    }

    pub fn viewportResize(self: *PhuxProvider, terminal_ref: provider.TerminalRef, viewport: provider.Viewport) !void {
        try self.host.viewportResize(terminal_ref, viewport);
    }
    pub fn sendKey(self: *PhuxProvider, owner_value: provider.ReplicaOwner, input: *const provider.KeyInput) !void {
        return self.host.sendKey(owner_value, input);
    }
    pub fn sendMouse(self: *PhuxProvider, owner_value: provider.ReplicaOwner, input: *const provider.MouseInput) !void {
        return self.host.sendMouse(owner_value, input);
    }
    pub fn mouseTracking(self: *const PhuxProvider, owner_value: provider.ReplicaOwner) !bool {
        return self.host.mouseTracking(owner_value);
    }

    pub fn mouseMode(self: *const PhuxProvider, owner_value: provider.ReplicaOwner) !provider.MouseMode {
        return self.host.mouseMode(owner_value);
    }

    pub fn selectionGesture(self: *PhuxProvider, owner_value: provider.ReplicaOwner, event: provider.SelectionGesture) !provider.SelectionGestureResult {
        return self.host.selectionGesture(owner_value, event);
    }
    pub fn sendFocus(self: *PhuxProvider, owner_value: provider.ReplicaOwner, focused: bool) !void {
        return self.host.sendFocus(owner_value, focused);
    }
    pub fn sendPaste(self: *PhuxProvider, owner_value: provider.ReplicaOwner, payload: []const u8, trusted: bool) !void {
        return self.host.sendPaste(owner_value, payload, trusted);
    }
    pub fn scrollViewport(self: *PhuxProvider, owner_value: provider.ReplicaOwner, scroll: provider.Scroll) !void {
        return self.host.scrollViewport(owner_value, scroll);
    }
    pub fn createAnchor(self: *PhuxProvider, owner_value: provider.ReplicaOwner, point: DocumentPoint) !Anchor {
        return self.host.createAnchor(owner_value, point);
    }
    pub fn releaseAnchor(self: *PhuxProvider, owner_value: provider.ReplicaOwner, anchor: Anchor) void {
        self.host.releaseAnchor(owner_value, anchor);
    }
    pub fn pinViewport(self: *PhuxProvider, owner_value: provider.ReplicaOwner, anchor: Anchor) !void {
        return self.host.pinViewport(owner_value, anchor);
    }
    pub fn clearPresentation(self: *PhuxProvider, owner_value: provider.ReplicaOwner) !void {
        return self.host.clearPresentation(owner_value);
    }
    pub fn setSelection(self: *PhuxProvider, owner_value: provider.ReplicaOwner, start_anchor: Anchor, end_anchor: Anchor, rectangle: bool) !void {
        return self.host.setSelection(owner_value, start_anchor, end_anchor, rectangle);
    }
    pub fn clearSelection(self: *PhuxProvider, owner_value: provider.ReplicaOwner) !void {
        return self.host.clearSelection(owner_value);
    }
    pub fn search(self: *PhuxProvider, owner_value: provider.ReplicaOwner, query: []const u8) ![]const SearchResult {
        return self.host.search(owner_value, query);
    }
    pub fn clearSearchResults(self: *PhuxProvider, expected_owner: ?provider.ReplicaOwner) void {
        self.host.clearSearchResults(expected_owner);
    }
    pub fn selectionText(self: *PhuxProvider, owner_value: provider.ReplicaOwner, gpa: std.mem.Allocator) ![]u8 {
        return self.host.selectionText(owner_value, gpa);
    }
    pub fn takeNotice(self: *PhuxProvider) ?Notice {
        return self.host.takeNotice();
    }

    pub fn phase(self: *const PhuxProvider, ref: provider.TerminalRef) ?provider.Phase {
        return self.host.phase(ref);
    }

    pub fn bellRung(self: *const PhuxProvider, ref: provider.TerminalRef) bool {
        return self.host.bellRung(ref);
    }

    pub fn atPrompt(self: *const PhuxProvider, ref: provider.TerminalRef) bool {
        return self.host.atPrompt(ref);
    }
    pub fn promptReturned(self: *const PhuxProvider, ref: provider.TerminalRef) bool {
        return self.host.promptReturned(ref);
    }
    pub fn acknowledgePromptReturn(self: *PhuxProvider, ref: provider.TerminalRef) void {
        self.host.acknowledgePromptReturn(ref);
    }
    pub fn bracketedPaste(self: *const PhuxProvider, owner_value: provider.ReplicaOwner) !bool {
        return self.host.bracketedPaste(owner_value);
    }

    pub fn takeEnded(self: *PhuxProvider) ?provider.TerminalRef {
        return self.host.takeEnded();
    }

    pub fn latchCommandFinished(self: *PhuxProvider, owner_value: provider.ReplicaOwner) bool {
        return self.host.latchCommandFinished(owner_value);
    }

    pub fn acknowledgeCommandFinished(self: *PhuxProvider, ref: provider.TerminalRef) void {
        self.host.acknowledgeCommandFinished(ref);
    }

    pub fn acknowledgeAllCommandsFinished(self: *PhuxProvider) void {
        self.host.acknowledgeAllCommandsFinished();
    }

    pub fn ringBell(self: *PhuxProvider, owner_value: provider.ReplicaOwner) bool {
        return self.host.ringBell(owner_value);
    }

    pub fn acknowledgeBell(self: *PhuxProvider, ref: provider.TerminalRef) void {
        self.host.acknowledgeBell(ref);
    }
    pub fn releaseNotice(self: *PhuxProvider, notice: Notice) void {
        self.host.releaseNotice(notice);
    }
};

test "a refused reconnect leaves the old generation frozen" {
    // A bare address has no runtime lane, so the reconnect is refused before
    // anything is dialed or supervised. What matters is what the refusal
    // leaves behind: the previous generation's canvas, frozen, not dropped.
    const self = try PhuxProvider.create(
        std.testing.allocator,
        std.testing.io,
        .{ .tcp = .{ .host = "127.0.0.1", .port = 4242 } },
        "session",
        "cockpit",
    );
    defer self.destroy();

    const id = try provider.RemoteResourceId.fromPhux(0, 11, "");
    try self.host.terminals.append(self.gpa, .{
        .id = id,
        .phase = .live,
        .published = true,
    });
    const terminal_ref: provider.TerminalRef = .{
        .provider_id = .phux,
        .terminal_id = .{ .phux = id },
    };

    try std.testing.expectError(error.Unsupported, self.reconnect(undefined));
    const presentation_value = self.presentation(terminal_ref);
    try std.testing.expect(presentation_value != null);
    try std.testing.expectEqual(provider.Phase.frozen, presentation_value.?.phase);
}

test "session selection follows actual attachment and server-advertised stable ids" {
    const self = try PhuxProvider.create(
        std.testing.allocator,
        std.testing.io,
        .{ .unix = "/unused" },
        "session",
        "cockpit",
    );
    defer self.destroy();

    try std.testing.expectEqual(@as(?u32, null), self.selectedSessionId());
    try host_mod.test_support.attachHost(self.host);
    try discoverFixtureSessions(self);
    try std.testing.expectEqual(@as(usize, 2), self.sessionCatalog().len);
    try std.testing.expectEqual(@as(?u32, 1), self.selectedSessionId());
    try std.testing.expectError(error.InvalidIdentity, self.selectSession(99));
    try std.testing.expect(!try self.selectSession(1));
    try std.testing.expect(try self.selectSession(2));
    // Selection queues an intent; GET_STATE's global focus cannot complete it.
    try std.testing.expectEqual(@as(?u32, 1), self.selectedSessionId());
}

fn discoverFixtureSessions(self: *PhuxProvider) !void {
    _ = try self.requestWorkspaceRefresh();
    try host_mod.test_support.stageWorkspaceFixture(self.bridge, "workspace_refresh_metadata.bin");
    try host_mod.test_support.stageWorkspaceFixture(self.bridge, "workspace_refresh_state.bin");
    _ = try self.drainReadiness();
}

test "pending session switch back to attached session replaces the requested destination" {
    const self = try PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/unused" }, null, "switch-back");
    defer self.destroy();
    try host_mod.test_support.attachHost(self.host);
    try discoverFixtureSessions(self);
    try std.testing.expectEqual(@as(?u32, 1), self.selectedSessionId());
    try std.testing.expect(try self.selectSession(2));
    try restartFixtureConnection(self);
    try std.testing.expect(self.attach_queued);
    try std.testing.expectEqual(State.negotiated, self.state());
    try std.testing.expectEqual(@as(?u32, 1), self.selectedSessionId());
    try std.testing.expectEqual(@as(?u32, 2), self.session_id);

    // B has been queued, but its ATTACHED has not arrived. Selecting A must
    // replace B's intent even though the last completed attachment is still A.
    try std.testing.expect(try self.selectSession(1));
    try std.testing.expectEqual(@as(?u32, 1), self.session_id);
    try std.testing.expect(!try self.selectSession(1));
    try restartFixtureConnection(self);
    try host_mod.test_support.stageFixture(self.bridge, "attached.bin");
    _ = try self.drainReadiness();
    try std.testing.expectEqual(State.attached, self.state());
    try std.testing.expectEqual(@as(?u32, 1), self.selectedSessionId());
    try std.testing.expectEqual(@as(?u32, 1), self.session_id);
}

fn restartFixtureConnection(self: *PhuxProvider) !void {
    self.prepareSessionSwitch();
    try self.host.reconnect(self.client_name);
    try host_mod.test_support.stageFixture(self.bridge, "hello.bin");
    _ = try self.drainReadiness();
}

test "provider lookups keep remote identity across reordered enumeration" {
    const self = try PhuxProvider.create(
        std.testing.allocator,
        std.testing.io,
        .{ .unix = "/unused" },
        "session",
        "cockpit",
    );
    defer self.destroy();

    const first_id = try provider.RemoteResourceId.fromPhux(0, 51, "");
    const second_id = try provider.RemoteResourceId.fromPhux(1, 51, "satellite");
    try self.host.terminals.append(self.gpa, .{
        .id = first_id,
        .generation = .{ .stream_id = 10, .bootstrap_id = 11 },
        .phase = .live,
        .published = true,
    });
    try self.host.terminals.append(self.gpa, .{
        .id = second_id,
        .generation = .{ .stream_id = 12, .bootstrap_id = 13 },
        .phase = .live,
        .published = true,
    });

    var first_order: [2]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 2), self.terminalRefs(&first_order));
    const first_owner = self.owner(first_order[0]).?;
    const second_owner = self.owner(first_order[1]).?;

    std.mem.swap(
        @TypeOf(self.host.terminals.items[0]),
        &self.host.terminals.items[0],
        &self.host.terminals.items[1],
    );
    var second_order: [2]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 2), self.terminalRefs(&second_order));
    try std.testing.expect(first_order[0].eql(second_order[1]));
    try std.testing.expect(first_order[1].eql(second_order[0]));

    try std.testing.expect(self.contains(first_owner.terminal_ref));
    try std.testing.expect(self.contains(second_owner.terminal_ref));
    try std.testing.expect(self.presentation(first_owner.terminal_ref).?.owner.eql(first_owner));
    try std.testing.expect(self.presentation(second_owner.terminal_ref).?.owner.eql(second_owner));
}

test "session switch clears old slots while same-session reconnect retains last-good canvas" {
    const self = try PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/unused" }, null, "test");
    defer self.destroy();
    try host_mod.test_support.attachHost(self.host);
    try discoverFixtureSessions(self);
    const before = try self.gpa.dupe(u8, self.host.terminals.items[0].canvas.screen_text.items);
    defer self.gpa.free(before);
    self.prepareSessionSwitch();
    try std.testing.expectEqual(@as(usize, 1), self.host.terminals.items.len);
    try std.testing.expectEqualStrings(before, self.host.terminals.items[0].canvas.screen_text.items);
    try std.testing.expect(try self.selectSession(2));
    self.prepareSessionSwitch();
    try std.testing.expectEqual(@as(usize, 0), self.host.terminals.items.len);
}

test "provider rejects a stale generation before forwarding host input" {
    const self = try PhuxProvider.create(
        std.testing.allocator,
        std.testing.io,
        .{ .unix = "/unused" },
        "session",
        "cockpit",
    );
    defer self.destroy();

    const id = try provider.RemoteResourceId.fromPhux(0, 52, "");
    try self.host.terminals.append(self.gpa, .{
        .id = id,
        .generation = .{ .stream_id = 20, .bootstrap_id = 21, .last_seq = 1 },
        .phase = .live,
        .published = true,
    });
    const terminal_ref: provider.TerminalRef = .{
        .provider_id = .phux,
        .terminal_id = .{ .phux = id },
    };
    const stale = self.owner(terminal_ref).?;

    self.host.terminals.items[0].generation.last_seq = 99;
    try std.testing.expect(self.ownerIsCurrent(stale));
    self.host.terminals.items[0].generation.bootstrap_id += 1;
    try std.testing.expect(!self.ownerIsCurrent(stale));
    try std.testing.expectError(error.InvalidState, self.sendFocus(stale, true));
}

test "provider stop freezes every published canvas without dropping refs" {
    const self = try PhuxProvider.create(
        std.testing.allocator,
        std.testing.io,
        .{ .unix = "/unused" },
        "session",
        "cockpit",
    );
    defer self.destroy();

    const first_id = try provider.RemoteResourceId.fromPhux(0, 53, "");
    const second_id = try provider.RemoteResourceId.fromPhux(0, 54, "");
    try self.host.terminals.append(self.gpa, .{ .id = first_id, .phase = .live, .published = true });
    try self.host.terminals.append(self.gpa, .{ .id = second_id, .phase = .live, .published = true });
    var before: [2]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 2), self.terminalRefs(&before));

    self.stop();

    var after: [2]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 2), self.terminalRefs(&after));
    for (before, after) |old_ref, new_ref| {
        try std.testing.expect(old_ref.eql(new_ref));
        const presentation_value = self.presentation(old_ref).?;
        try std.testing.expectEqual(provider.Phase.frozen, presentation_value.phase);
        try std.testing.expect(!presentation_value.grid.running);
    }
}

test "every declaration in this module is compiled, not merely reachable" {
    // Zig analyzes only what is referenced, so a module can sit in the build
    // graph with its signatures never checked. Nothing calls PhuxProvider.search.
    // See ref.zig.
    @import("phux_ref").refAllDeclsRecursive(@This());
}

test "retarget waits for the next connection, then forgets the old coordinator's session" {
    const self = try PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/unused" }, "local-session", "retarget");
    defer self.destroy();
    try host_mod.test_support.attachHost(self.host);
    try discoverFixtureSessions(self);
    try std.testing.expectEqual(@as(usize, 1), self.host.terminals.items.len);
    try std.testing.expect(self.remoteTarget() == null);
    self.remote_status.noteConnected();

    try self.requestRetarget(.{ .remote = .{ .target = "me@mini" } }, "work", "mini");
    // Visible to the status line at once, applied to nothing yet.
    try std.testing.expectEqualStrings("me@mini", self.remoteTarget().?);
    try std.testing.expectEqualStrings("mini", self.remoteLabel().?);
    try std.testing.expectEqualStrings("/unused", self.endpointDescriptor().unix);
    try std.testing.expect(!self.remoteConnectedOnce());

    self.applyPendingRetarget();
    try std.testing.expectEqualStrings("me@mini", self.endpointDescriptor().remote.target);
    try std.testing.expectEqualStrings("work", self.session.?);
    try std.testing.expect(self.session_id == null);
    try std.testing.expectEqual(@as(usize, 0), self.host.terminals.items.len);
    try std.testing.expect(self.workerEndpoint().remote.status == &self.remote_status);

    // Back to the local coordinator: no remote label survives.
    try self.requestRetarget(.{ .unix = "/local.sock" }, null, null);
    self.applyPendingRetarget();
    try std.testing.expect(self.remoteTarget() == null);
    try std.testing.expect(self.remoteLabel() == null);
    try std.testing.expect(self.session == null);
}

test "a provider created for a remote host labels it by its target until resolved" {
    const self = try PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .remote = .{ .target = "mini" } }, null, "remote");
    defer self.destroy();
    try std.testing.expectEqualStrings("mini", self.remoteLabel().?);
    try std.testing.expectEqualStrings("mini", self.endpointDescriptor().remote.target);
}

test "provider operations expose owned outcomes and borrowed endpoint incarnation" {
    var path = [_]u8{ '/', 's', 'o', 'c', 'k', 'e', 't' };
    const self = try PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = &path }, null, "operations-test");
    defer self.destroy();
    path[1] = 'X';
    try std.testing.expectEqualStrings("/socket", self.endpointDescriptor().unix);
    try std.testing.expect(self.serverId() == null);
    try std.testing.expectError(error.InvalidState, self.requestSpawn(null, .{ .cols = 80, .rows = 24 }));
    try host_mod.test_support.attachHost(self.host);
    try std.testing.expectEqualStrings("cockpit-fixture", self.serverId().?);
    const epoch = self.connectionEpoch();
    const request_id = try self.requestSpawn(null, .{ .cols = 80, .rows = 24 });
    try host_mod.test_support.stageFixture(self.bridge, "spawn-local.bin");
    _ = try self.drainReadiness();
    const result = self.takeOperationResult().?;
    try std.testing.expectEqual(request_id, result.request_id);
    try std.testing.expectEqual(epoch, result.connection_epoch);
    try std.testing.expect(!self.contains(result.terminal_ref.?));
    try host_mod.test_support.stageFixture(self.bridge, "local-ready.bin");
    _ = try self.drainReadiness();
    try std.testing.expect(self.contains(result.terminal_ref.?));
    const pending = try self.requestSpawn(result.terminal_ref, .{ .cols = 80, .rows = 24 });
    self.stop();
    const unknown = self.takeOperationResult().?;
    try std.testing.expectEqual(pending, unknown.request_id);
    try std.testing.expectEqual(.unknown_outcome, unknown.status);
    try std.testing.expectEqual(epoch, unknown.connection_epoch);
}

test "the connected lane dials a socket or a registered host, and refuses a bare address" {
    const local = try PhuxProvider.create(
        std.testing.allocator,
        std.testing.io,
        .{ .unix = "/tmp/phux-lane.sock" },
        null,
        "cockpit",
    );
    defer local.destroy();
    const local_target = try local.connectTarget();
    try std.testing.expectEqualStrings("/tmp/phux-lane.sock", local_target.socket_path);
    try std.testing.expectEqual(@as(usize, 0), local_target.target.len);

    const remote = try PhuxProvider.create(
        std.testing.allocator,
        std.testing.io,
        .{ .remote = .{ .target = "mini", .config_path = "/etc/phux.toml" } },
        null,
        "cockpit",
    );
    defer remote.destroy();
    const remote_target = try remote.connectTarget();
    try std.testing.expectEqualStrings("mini", remote_target.target);
    try std.testing.expectEqualStrings("/etc/phux.toml", remote_target.config_path);
    try std.testing.expectEqual(@as(usize, 0), remote_target.socket_path.len);

    // A bare address carries neither a pin nor a token, and the registry is
    // what supplies both. It stays the embedded lane's.
    const bare = try PhuxProvider.create(
        std.testing.allocator,
        std.testing.io,
        .{ .tcp = .{ .host = "10.0.0.2", .port = 4242 } },
        null,
        "cockpit",
    );
    defer bare.destroy();
    try std.testing.expectError(error.Unsupported, bare.connectTarget());
}

test "a stopped wake context posts nothing, and a null one is inert" {
    var context: WakeContext = .{};
    // No handle: the provider has not opened yet.
    connectedWake(&context);
    // Stopped: the degraded teardown path, where a driver may outlive stop.
    context.stopped.store(true, .release);
    connectedWake(&context);
    // The runtime never passes null, but a callback that dereferences one
    // would be a crash in someone else's thread.
    connectedWake(null);
}

test "a provider that never opened tears down without a connection" {
    const self = try PhuxProvider.create(
        std.testing.allocator,
        std.testing.io,
        .{ .unix = "/unused" },
        null,
        "cockpit",
    );
    defer self.destroy();
    // Nothing has connected, so the host still holds the embedded client it
    // was created with; stop must be a safe no-op rather than a refusal.
    try std.testing.expectEqual(host_mod.Lane.embedded, self.host.lane);
    self.stop();
    try std.testing.expectEqual(host_mod.Lane.embedded, self.host.lane);
}

/// A real connected handle, driven by canonical server frames on a disposable
/// UDS. In particular, no helper clears the provider's scheduling latches.
const ConnectedFixture = struct {
    paths: extension.startup.TestFixture,
    listener: c_int,
    client: c_int = -1,
    frame: [65536]u8 = undefined,

    fn init() !ConnectedFixture {
        var paths = try extension.startup.TestFixture.init();
        errdefer paths.deinit();
        var address = std.mem.zeroes(std.posix.sockaddr.un);
        address.len = @intCast(@offsetOf(std.posix.sockaddr.un, "path") + paths.socket.len + 1);
        address.family = std.posix.AF.UNIX;
        @memcpy(address.path[0..paths.socket.len], paths.socket);
        const listener = std.c.socket(std.posix.AF.UNIX, std.posix.SOCK.STREAM, 0);
        try std.testing.expect(listener >= 0);
        errdefer _ = std.c.close(listener);
        try std.testing.expectEqual(@as(c_int, 0), std.c.bind(listener, @ptrCast(&address), address.len));
        try std.testing.expectEqual(@as(c_int, 0), std.c.listen(listener, 8));
        return .{ .paths = paths, .listener = listener };
    }

    fn deinit(self: *ConnectedFixture) void {
        if (self.client >= 0) _ = std.c.close(self.client);
        _ = std.c.close(self.listener);
        self.paths.deinit();
    }

    fn readable(fd: c_int) !void {
        var polls = [_]std.posix.pollfd{.{ .fd = fd, .events = std.posix.POLL.IN, .revents = 0 }};
        if (try std.posix.poll(&polls, 10000) != 1) return error.ConnectedFixtureTimedOut;
    }

    fn readAll(fd: c_int, buffer: []u8) !void {
        var offset: usize = 0;
        while (offset < buffer.len) {
            try readable(fd);
            const count = std.c.read(fd, buffer[offset..].ptr, buffer.len - offset);
            if (count == 0) return error.EndOfStream;
            if (count < 0) return error.SocketRead;
            offset += @intCast(count);
        }
    }

    fn readFrame(self: *ConnectedFixture) ![]const u8 {
        try readAll(self.client, self.frame[0..4]);
        const size = std.mem.readInt(u32, self.frame[0..4], .big);
        if (size == 0 or size > self.frame.len - 4) return error.InvalidFixtureFrame;
        try readAll(self.client, self.frame[4..][0..size]);
        return self.frame[0 .. 4 + size];
    }

    fn acceptClient(self: *ConnectedFixture) !void {
        if (self.client >= 0) _ = std.c.close(self.client);
        self.client = -1;
        // Local startup probes the listener once before the runtime dials.
        // That connection closes without HELLO; it is not the client.
        while (true) {
            try readable(self.listener);
            self.client = std.c.accept(self.listener, null, null);
            try std.testing.expect(self.client >= 0);
            const frame = self.readFrame() catch |err| {
                _ = std.c.close(self.client);
                self.client = -1;
                if (err == error.EndOfStream) continue;
                return err;
            };
            try std.testing.expectEqual(@as(u8, 0x01), frame[4]);
            return;
        }
    }

    fn send(self: *ConnectedFixture, value: []const u8) !void {
        var offset: usize = 0;
        while (offset < value.len) {
            const count = std.c.write(self.client, value[offset..].ptr, value.len - offset);
            if (count <= 0) return error.SocketWrite;
            offset += @intCast(count);
        }
    }

    fn fixture(self: *ConnectedFixture, name: []const u8) !void {
        const value = try host_mod.test_support.readFixture(name);
        defer std.testing.allocator.free(value);
        try self.send(value);
    }

    fn hello(self: *ConnectedFixture, foreign: bool) !void {
        const value = try host_mod.test_support.readFixture("hello.bin");
        defer std.testing.allocator.free(value);
        if (foreign) {
            const offset = std.mem.indexOf(u8, value, "cockpit-fixture") orelse return error.MissingServerIdentity;
            @memcpy(value[offset..][0.."foreign-server!".len], "foreign-server!");
        }
        try self.send(value);
    }

    fn until(self: *ConnectedFixture, kind: u8) ![]const u8 {
        for (0..32) |_| {
            const frame = try self.readFrame();
            if (frame[4] == kind) return frame;
        }
        return error.MissingExpectedFrame;
    }

    fn expectAttach(self: *ConnectedFixture, kind: u8, id: u32, name: []const u8) !void {
        const frame = try self.until(0x02);
        // ATTACH's first TLV is TARGET: id 1, BYTES, one-byte length for
        // these small targets. Inspect the target, not a substring in a frame.
        try std.testing.expectEqualSlices(u8, &.{ 1, 4 }, frame[5..7]);
        try std.testing.expectEqual(kind, frame[8]);
        switch (kind) {
            1 => {
                try std.testing.expectEqual(name.len, std.mem.readInt(u32, frame[9..13], .big));
                try std.testing.expectEqualStrings(name, frame[13..][0..name.len]);
            },
            2 => try std.testing.expectEqual(id, std.mem.readInt(u32, frame[9..13], .big)),
            else => try std.testing.expectEqual(@as(u8, 0), kind),
        }
    }

    fn expectQuery(self: *ConnectedFixture) !void {
        for (0..32) |_| {
            const frame = try self.readFrame();
            try std.testing.expect(frame[4] != 0x02);
            // COMMAND's request-id TLV precedes its command TLV.
            if (frame[4] == 0x31 and frame.len > 15 and frame[15] == 0x05) return;
        }
        return error.MissingSessionQuery;
    }

    const Await = enum { negotiated, attached, catalog, workspace };

    fn awaitProvider(_: *ConnectedFixture, remote: *PhuxProvider, target: Await) !void {
        const started = std.Io.Clock.awake.now(std.testing.io);
        while (true) {
            _ = try remote.drainReadiness();
            const ready = switch (target) {
                .negotiated => remote.state() == .negotiated,
                .attached => remote.state() == .attached,
                .catalog => remote.standbyCatalog().len != 0,
                .workspace => remote.workspaceSnapshot().status == .confirmed,
            };
            if (ready) return;
            if (started.durationTo(std.Io.Clock.awake.now(std.testing.io)).toMilliseconds() >= 10000)
                return error.ProviderRecoveryTimedOut;
            try std.Io.sleep(std.testing.io, .fromMilliseconds(1), .awake);
        }
    }

    fn finishAttach(self: *ConnectedFixture, remote: *PhuxProvider) !void {
        try self.fixture("attached.bin");
        try self.awaitProvider(remote, .attached);
        for ([_][]const u8{ "workspace_initial_metadata.bin", "workspace_initial_state.bin" }, 0..) |name, index| {
            const path = try std.fmt.allocPrint(std.testing.allocator, "src/providers/phux/fixtures/{s}", .{name});
            defer std.testing.allocator.free(path);
            const value = try std.Io.Dir.cwd().readFileAlloc(std.testing.io, path, std.testing.allocator, .limited(65536));
            defer std.testing.allocator.free(value);
            // A retained ABI handle keeps request IDs monotonic. Reply to the
            // current read, not the first connection's fixture correlation.
            const request = try self.until(if (index == 0) 0x50 else 0x31);
            try std.testing.expect(request.len >= 12 and value.len >= 12);
            try std.testing.expectEqualSlices(u8, &.{ 1, 4, 4 }, request[5..8]);
            try std.testing.expectEqualSlices(u8, &.{ 1, 4, 4 }, value[5..8]);
            @memcpy(value[8..12], request[8..12]);
            try self.send(value);
        }
        try self.awaitProvider(remote, .workspace);
    }
};

test "connected stop open retires once and recovers attachment workspace and input" {
    var wire = try ConnectedFixture.init();
    defer wire.deinit();
    const self = try PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = wire.paths.socket }, null, "replacement");
    defer self.destroy();
    try self.open(.{});
    try wire.acceptClient();
    try wire.hello(false);
    try wire.awaitProvider(self, .negotiated);
    try wire.expectAttach(0, 0, "");
    try wire.finishAttach(self);
    var refs: [1]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 1), self.terminalRefs(&refs));
    const old_owner = self.owner(refs[0]).?;
    const generation = self.connectionEpoch();
    const old_canvas = try self.gpa.dupe(u8, self.presentation(refs[0]).?.grid.screen_text);
    defer self.gpa.free(old_canvas);
    try std.testing.expect(std.mem.startsWith(u8, old_canvas, "COCKPIT FIXTURE"));

    self.stop();
    self.stop(); // Engine close continuations can stop an already stopped host.
    try std.testing.expectEqual(generation + 1, self.connectionEpoch());
    try std.testing.expect(!self.ownerIsCurrent(old_owner));
    try std.testing.expectEqualStrings(old_canvas, self.presentation(refs[0]).?.grid.screen_text);
    try std.testing.expectEqual(@as(usize, 0), self.standbyCatalog().len);
    try self.open(.{});
    try wire.acceptClient();
    try wire.hello(false);
    try wire.awaitProvider(self, .negotiated);
    try std.testing.expectEqual(generation + 1, self.connectionEpoch());
    try std.testing.expect(!self.ownerIsCurrent(old_owner));
    try wire.expectAttach(2, 1, "");
    try wire.finishAttach(self);
    try std.testing.expectEqualStrings("fixture", self.sessionCatalog()[0].name);
    try std.testing.expectEqual(@as(?u32, 1), self.selectedSessionId());
    const current = self.owner(refs[0]).?;
    try std.testing.expect(self.ownerIsCurrent(current));
    try std.testing.expect(!self.ownerIsCurrent(old_owner));
    try self.sendPaste(current, "after replacement", true);
    const input = try wire.until(0x11);
    try std.testing.expect(std.mem.indexOf(u8, input, "after replacement") != null);

    // Same-handle resync retires immediately too: a drain before the runtime
    // processes it must not publish the old C client's READY a second time.
    try self.reconnect(.{});
    try std.testing.expect(self.state() != .attached);
    const waiting = try self.drainReadiness();
    try std.testing.expect(!waiting.ready_published);
    try std.testing.expect(!self.ownerIsCurrent(current));
    try wire.acceptClient();
    try wire.hello(false);
    try wire.awaitProvider(self, .negotiated);
    try std.testing.expectEqual(generation + 2, self.connectionEpoch());
    try wire.expectAttach(2, 1, "");
    try wire.finishAttach(self);
    try std.testing.expect(self.ownerIsCurrent(self.owner(refs[0]).?));
}

test "connected standby stop open lists again and showing peer reattaches by incarnation" {
    var wire = try ConnectedFixture.init();
    defer wire.deinit();
    const self = try PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = wire.paths.socket }, null, "standby-replacement");
    defer self.destroy();
    self.standBy();
    for (0..2) |cycle| {
        try self.open(.{});
        try wire.acceptClient();
        try wire.hello(false);
        try wire.awaitProvider(self, .negotiated);
        try wire.expectQuery();
        try wire.fixture("standby_state.bin");
        try wire.awaitProvider(self, .catalog);
        try std.testing.expectEqual(@as(usize, 2), self.standbyCatalog().len);
        try std.testing.expectEqualStrings("build", self.standbyCatalog()[0].name);
        if (cycle == 0) {
            self.stop();
            try std.testing.expectEqual(@as(usize, 0), self.standbyCatalog().len);
        }
    }
    // Showing peers use the same owner, not an Engine-only latch reset.
    try self.show(1);
    self.stop();
    try self.open(.{});
    try wire.acceptClient();
    try wire.hello(true);
    try wire.awaitProvider(self, .negotiated);
    // On the replacement server ID 1 may name an unrelated session. The
    // request must select the previously listed logical name instead.
    try wire.expectAttach(1, 0, "build");
}

test "replacement server cannot reuse selected id or invent unnamed recovery intent" {
    const self = try PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/unused" }, "startup-is-not-a-fallback", "incarnation");
    defer self.destroy();
    try host_mod.test_support.attachHost(self.host);
    _ = try self.drainReadiness();
    // A rename observed after attachment is the logical reconnect name.
    try host_mod.test_support.stageFixture(self.bridge, "session_renamed.bin");
    _ = try self.drainReadiness();
    var refs: [1]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 1), self.terminalRefs(&refs));
    const old_owner = self.owner(refs[0]).?;
    try self.host.reconnect(self.client_name);
    try self.host.sendKey(old_owner, &.{ .action = .press, .physical = @enumFromInt(0), .text = "blind typing" });
    const hello = try host_mod.test_support.readFixture("hello.bin");
    defer std.testing.allocator.free(hello);
    const offset = std.mem.indexOf(u8, hello, "cockpit-fixture") orelse return error.MissingServerIdentity;
    @memcpy(hello[offset..][0.."foreign-server!".len], "foreign-server!");
    try std.testing.expect(self.bridge.incoming.stage(hello));
    _ = try self.drainReadiness();
    try std.testing.expectError(error.InvalidState, self.host.sendKey(old_owner, &.{ .action = .press, .physical = @enumFromInt(0), .text = "stale input" }));
    var attached = false;
    while (self.bridge.outgoing.take()) |frame| {
        defer self.bridge.outgoing.release(frame);
        if (frame[4] != 0x02) continue;
        attached = true;
        try std.testing.expectEqual(@as(u8, 1), frame[8]);
        try std.testing.expectEqualStrings("renamed", frame[13..][0..7]);
    }
    try std.testing.expect(attached);
    try host_mod.test_support.stageFixture(self.bridge, "attached.bin");
    _ = try self.drainReadiness();
    while (self.bridge.outgoing.take()) |frame| {
        defer self.bridge.outgoing.release(frame);
        try std.testing.expect(frame[4] != 0x10);
    }

    // A known ID with no authoritative name cannot become LAST, a configured
    // startup name, or a numeric request to the foreign coordinator.
    const unnamed = try PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/unused" }, "not-a-fallback", "unnamed");
    defer unnamed.destroy();
    try unnamed.show(1);
    try unnamed.host.start(unnamed.client_name);
    try host_mod.test_support.stageFixture(unnamed.bridge, "hello.bin");
    _ = try unnamed.drainReadiness();
    try unnamed.host.reconnect(unnamed.client_name);
    unnamed.bridge.outgoing.reset();
    try std.testing.expect(unnamed.bridge.incoming.stage(hello));
    try std.testing.expectError(error.InvalidIdentity, unnamed.drainReadiness());
    while (unnamed.bridge.outgoing.take()) |frame| {
        defer unnamed.bridge.outgoing.release(frame);
        try std.testing.expect(frame[4] != 0x02);
    }
}
