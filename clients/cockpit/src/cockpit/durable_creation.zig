//! Command acceptance is not a replica. Keep placement intent until the exact
//! returned terminal publishes, without following a later focus or retrying.
const model_module = @import("model.zig");
const support = @import("phux_support.zig");
const layout = @import("layout.zig");
const contract = @import("provider_contract");
const shared_mutations = @import("shared_mutations.zig");
const results = @import("command_results.zig");
const session_navigation = @import("session_navigation.zig");
const Model = model_module.Model;
const TerminalRef = contract.TerminalRef;

const SpawnSource = struct {
    provider: u64,
    host: u64,

    fn matches(self: SpawnSource, remote: *const support.PhuxProvider) bool {
        return self.provider == remote.context_id and self.host == remote.host.context_id;
    }
};

pub const Kind = enum { tab, window, split_right, split_down };
const Pending = struct {
    navigation: ?session_navigation.Navigation = null,
    command_id: ?u64 = null,
    operation_request: u32 = 0,
    operation: ?results.Operation = null,
    completion: ?results.Result = null,
    projected_id: ?[16]u8 = null,
    placement_request: u32 = 0,
    mutation_identity: u64 = 0,
    mutation_outcome: ?results.Operation = null,
    attach_request: u32 = 0,
    error_domain: u32 = 0,
    error_code: u32 = 0,
    placement_window: ?usize = null,
    placement_window_epoch: u64 = 0,
    request: u32 = 0,
    epoch: u64 = 0,
    window: usize,
    window_epoch: u64,
    kind: Kind,
    origin: ?TerminalRef,
    terminal: ?TerminalRef = null,
    accepted: bool = false,
    session: u32 = 0,
    shared_window: ?[16]u8 = null,
    mutation_ticket: ?u64 = null,
    attach_only: bool = false,
    existing_member: bool = false,
    focus: ?TerminalRef = null,
    focus_window: ?usize = null,
    may_focus: bool = true,
    /// Only adopted bound spawns transfer conditional cleanup ownership here.
    instance: ?[16]u8 = null,
    spawn_source: ?SpawnSource = null,
};

pub const Creation = struct {
    pending: [16]?Pending = @splat(null),

    pub fn count(self: *const Creation) usize {
        var n: usize = 0;
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.completion != null) continue;
            n += 1;
        }
        return n;
    }

    /// First-tab evidence for the active provider this drain. Entries still
    /// advancing report `.pending`; completed placements report their result.
    /// `empty_session.settleAttachment` consumes opening flags from these
    /// outcomes without treating an empty queue as refusal.
    pub const EmptyFirstTab = struct {
        window: usize,
        window_epoch: u64,
        connection_epoch: u64,
        outcome: Outcome,

        pub const Outcome = enum { pending, refused, placed };
    };

    pub fn collectEmptyFirstTabs(self: *const Creation, out: []EmptyFirstTab) usize {
        var total: usize = 0;
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (total >= out.len) break;
            if (entry.completion) |completion| {
                const outcome: EmptyFirstTab.Outcome = switch (completion.placement) {
                    .placed => .placed,
                    .refused, .destination_lost, .unknown => .refused,
                    .not_requested => continue,
                };
                out[total] = .{
                    .window = entry.placement_window orelse entry.window,
                    .window_epoch = if (entry.placement_window != null) entry.placement_window_epoch else entry.window_epoch,
                    .connection_epoch = entry.epoch,
                    .outcome = outcome,
                };
            } else {
                out[total] = .{
                    .window = entry.placement_window orelse entry.window,
                    .window_epoch = if (entry.placement_window != null) entry.placement_window_epoch else entry.window_epoch,
                    .connection_epoch = entry.epoch,
                    .outcome = .pending,
                };
            }
            total += 1;
        }
        return total;
    }

    fn vacant(self: *Creation) !*?Pending {
        for (&self.pending) |*slot| if (slot.* == null) return slot;
        return error.OperationCapacity;
    }

    pub fn hasPendingTerminal(self: *const Creation, ref: TerminalRef) bool {
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.completion != null) continue;
            if (entry.terminal) |terminal| if (terminal.eql(ref)) return true;
        }
        return false;
    }

    pub fn request(self: *Creation, model: *Model, kind: Kind) !void {
        return self.spawn(model, kind, null, "", .focused);
    }

    /// A new tab or window whose shell starts in `cwd` (Go to Directory) on
    /// the host the directory was listed on. `owner` null: the connected
    /// coordinator's own host, spawned without an owner, so the focused
    /// terminal's satellite route cannot carry the directory to another
    /// host. `owner` a satellite pane: that satellite, routed through the
    /// hub's relay exactly as a split of that pane would be.
    pub fn requestIn(self: *Creation, model: *Model, kind: Kind, cwd: []const u8, owner: ?TerminalRef) !void {
        return self.spawn(model, kind, null, cwd, if (owner) |ref| .{ .terminal = ref } else .none);
    }

    pub fn requestCorrelated(self: *Creation, model: *Model, kind: Kind, command_id: u64) !void {
        return self.spawn(model, kind, command_id, "", .focused);
    }

    /// Transfer an already successful bound spawn into the normal publication
    /// pump. Errors leave the receipt and all cleanup authority with the caller.
    /// Once reserved, even synchronous attachment failure is a retained result;
    /// it must never return the same spawn to a second cleanup owner.
    pub fn adoptSpawnIn(self: *Creation, model: *Model, remote: *support.PhuxProvider, result: support.OperationResult, window: usize, epoch: u64, command_id: u64, may_focus: bool) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const ref = try validateAdoptedSpawn(model, remote, result);
        try self.requireUniqueCommand(command_id);
        _ = try self.duplicateTerminal(ref, command_id);
        const slot = try self.vacant();
        var entry = try self.prepareAdoption(model, window, epoch);
        entry.epoch = result.connection_epoch;
        entry.command_id = command_id;
        entry.operation_request = result.request_id;
        entry.terminal = ref;
        entry.instance = result.instance;
        entry.spawn_source = .{ .provider = remote.context_id, .host = remote.host.context_id };
        entry.may_focus = may_focus;
        slot.* = entry;
        acceptResult(model, &slot.*.?, result) catch |err| {
            finishFailure(model, &slot.*.?, failurePlacement(err), failureReason(err));
        };
        _ = self.pump(model);
    }

    fn prepareAdoption(self: *const Creation, model: *Model, window: usize, epoch: u64) !Pending {
        if (!model.windowOpen(window)) return error.StaleDestination;
        if (model.window_epochs[window] != epoch) return error.StaleDestination;
        if (epoch == @import("std").math.maxInt(u64)) return error.StaleDestination;
        if (!hasCapacity(model, self.count())) return error.TerminalCapacity;
        const workspace = model.wsAtConst(window) orelse return error.StaleDestination;
        try validateDestinationCapacity(workspace, .tab, self.reservedInWindow(model, window, .tab), true);
        return .{
            .window = window,
            .window_epoch = epoch,
            .kind = .tab,
            .origin = null,
            .session = model.shared_workspace.session,
            .focus = model.focusedTerminalRef(),
            .focus_window = model.active_window,
        };
    }

    /// Reserve command/result ownership before selectSession or leaveSession.
    /// The intentional disconnect commits the handoff and retires older work.
    pub fn reserveSessionCorrelated(self: *Creation, model: *Model, target_session: u32, terminal: ?TerminalRef, command_id: u64) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const remote = model.phux() orelse return error.NoProvider;
        try self.requireUniqueCommand(command_id);
        try self.requireSessionAvailable(target_session);
        const slot = try self.vacant();
        const navigation = try session_navigation.Navigation.capture(remote, target_session);
        var entry = try prepareDestination(model, .tab, 0, false);
        entry.command_id = command_id;
        entry.navigation = navigation;
        entry.session = target_session;
        entry.terminal = terminal;
        entry.attach_only = true;
        slot.* = entry;
    }

    fn requireSessionAvailable(self: *const Creation, target: u32) !void {
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.completion != null) continue;
            const navigation = entry.navigation orelse continue;
            if (navigation.target == target) return error.OperationBusy;
        }
    }

    fn sessionEntry(self: *Creation, command_id: u64) ?*Pending {
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.completion != null) continue;
            if (entry.navigation == null) continue;
            if (entry.command_id == command_id) return entry;
        }
        return null;
    }

    pub fn cancelSessionReservation(self: *Creation, command_id: u64) bool {
        const entry = self.sessionEntry(command_id) orelse return false;
        if (entry.navigation.?.phase != .waiting_old_close) return false;
        for (&self.pending) |*slot| {
            if (slot.* == null) continue;
            if (&slot.*.? != entry) continue;
            slot.* = null;
            return true;
        }
        return false;
    }

    pub fn bindSessionConnection(self: *Creation, model: *Model, command_id: u64) bool {
        const entry = self.sessionEntry(command_id) orelse return false;
        // A repeated callback cannot rebind or invalidate an already bound epoch.
        if (entry.navigation.?.phase != .waiting_old_close) return false;
        const remote = model.phux() orelse {
            self.failSessionAttempt(model, command_id);
            return false;
        };
        entry.navigation.?.bind(remote) catch {
            finishFailure(model, entry, .unknown, .context_changed);
            return false;
        };
        entry.epoch = remote.connectionEpoch();
        return true;
    }

    /// Accepted restart attempts must retire even when opening fails synchronously
    /// and therefore no replacement channel callback can ever arrive.
    pub fn failSessionAttempt(self: *Creation, model: *Model, command_id: u64) void {
        const entry = self.sessionEntry(command_id) orelse return;
        const child = retireMutation(model, entry);
        finishRetirement(model, entry, child, .disconnected);
    }

    pub fn observeSessionAttachment(self: *Creation, model: *Model) bool {
        const remote = model.phux() orelse return self.retireSessionContext(model);
        var changed = false;
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.completion != null) continue;
            const navigation = if (entry.navigation) |*value| value else continue;
            const attached = navigation.observeAttachment(remote) catch |err| {
                if (err == error.SessionMismatch and entry.operation == null) entry.operation = .refused;
                finishSessionObservationFailure(model, entry, err);
                changed = true;
                continue;
            };
            if (!attached) continue;
            entry.operation = .success;
            changed = true;
        }
        return changed;
    }

    fn retireSessionContext(self: *Creation, model: *Model) bool {
        var changed = false;
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.completion != null or entry.navigation == null) continue;
            finishSessionObservationFailure(model, entry, error.NoProvider);
            changed = true;
        }
        return changed;
    }

    /// Which terminal owns a spawn: the focused one (New Tab, a split), none
    /// (a coordinator-host directory), or an exact one (a satellite
    /// directory, owned by the pane it was listed for).
    const OwnerChoice = union(enum) { focused, none, terminal: TerminalRef };

    fn spawn(self: *Creation, model: *Model, kind: Kind, command_id: ?u64, cwd: []const u8, choice: OwnerChoice) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const remote = model.phux() orelse return error.NoProvider;
        if (remote.state() != .attached) return error.NotReady;
        try requireWorkspace(model);
        try self.requireUniqueCommand(command_id);
        if (!hasCapacity(model, self.count())) return error.TerminalCapacity;
        const slot = try self.vacant();
        const owner = switch (choice) {
            .focused => try focusedOwner(model, kind),
            .none => null,
            .terminal => |ref| try spawnOwner(model, ref),
        };
        var entry = try prepareDestination(model, kind, self.reservedAtDestination(model, kind), true);
        entry.epoch = remote.connectionEpoch();
        entry.command_id = command_id;
        slot.* = entry;
        errdefer slot.* = null;
        try allocateDestination(model, remote, entry);
        errdefer retireEmptyDestination(model, entry);
        slot.*.?.request = try remote.requestSpawnIn(owner, spawnViewport(remote, owner), cwd);
        slot.*.?.operation_request = slot.*.?.request;
        if (kind == .window) model.active_window = entry.window;
    }

    fn reservedAtDestination(self: *const Creation, model: *const Model, kind: Kind) usize {
        return self.reservedInWindow(model, model.active_window, kind);
    }

    fn reservedInWindow(self: *const Creation, model: *const Model, window: usize, kind: Kind) usize {
        const workspace = model.wsAtConst(window) orelse return 0;
        var reserved: usize = 0;
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.completion != null) continue;
            if (entry.window != window) continue;
            if (entry.window_epoch != model.window_epochs[entry.window]) continue;
            if (sharesCapacity(entry, workspace, kind)) reserved += 1;
        }
        return reserved;
    }

    /// Explicit navigation reuses the creation destination/epoch transaction;
    /// it never places a catalog-only identity before exact stream publication.
    pub fn requestAttach(self: *Creation, model: *Model, ref: TerminalRef) !void {
        return self.attach(model, ref, null);
    }

    pub fn requestAttachCorrelated(self: *Creation, model: *Model, ref: TerminalRef, command_id: u64) !void {
        return self.attach(model, ref, command_id);
    }

    fn attach(self: *Creation, model: *Model, ref: TerminalRef, command_id: ?u64) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const remote = model.phux() orelse return error.NoProvider;
        if (remote.state() != .attached) return error.NotReady;
        try requireWorkspace(model);
        try self.requireUniqueCommand(command_id);
        if (try self.duplicateTerminal(ref, command_id)) return;
        if (remote.presentation(ref)) |view| {
            if (view.phase == .live) return self.admit(model, ref, command_id);
        }
        const slot = try self.vacant();
        var entry = try self.prepareAttachment(model, ref);
        entry.epoch = remote.connectionEpoch();
        entry.command_id = command_id;
        slot.* = entry;
        errdefer slot.* = null;
        slot.*.?.request = try remote.requestAttach(ref);
        slot.*.?.operation_request = slot.*.?.request;
    }

    /// A live catalog terminal needs shared admission, not another subscription.
    pub fn requestAdmit(self: *Creation, model: *Model, ref: TerminalRef) !void {
        return self.admit(model, ref, null);
    }

    pub fn requestAdmitCorrelated(self: *Creation, model: *Model, ref: TerminalRef, command_id: u64) !void {
        return self.admit(model, ref, command_id);
    }

    fn admit(self: *Creation, model: *Model, ref: TerminalRef, command_id: ?u64) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const remote = model.phux() orelse return error.NoProvider;
        if (remote.state() != .attached) return error.NotReady;
        try requireWorkspace(model);
        try self.requireUniqueCommand(command_id);
        if (try self.duplicateTerminal(ref, command_id)) return;
        const view = remote.presentation(ref) orelse return error.NotReady;
        if (view.phase != .live) return error.NotReady;
        const slot = try self.vacant();
        var entry = try self.prepareAttachment(model, ref);
        entry.epoch = remote.connectionEpoch();
        entry.accepted = true;
        entry.command_id = command_id;
        entry.operation = .success;
        slot.* = entry;
        _ = self.pump(model);
    }

    fn duplicateTerminal(self: *const Creation, ref: TerminalRef, command_id: ?u64) !bool {
        if (!self.hasPendingTerminal(ref)) return false;
        if (command_id != null) return error.OperationBusy;
        return true;
    }

    fn requireUniqueCommand(self: *const Creation, command_id: ?u64) !void {
        const id = command_id orelse return;
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.command_id == id) return error.CommandBusy;
        }
    }

    fn prepareAttachment(self: *Creation, model: *Model, ref: TerminalRef) !Pending {
        const member = sharedMember(model, ref);
        if (!member and !hasCapacity(model, self.count())) return error.TerminalCapacity;
        var entry = try prepareDestination(model, .tab, self.reservedAtDestination(model, .tab), !member);
        entry.terminal = ref;
        entry.attach_only = true;
        entry.existing_member = member;
        if (member) captureMemberDestination(model, &entry);
        return entry;
    }

    pub fn pump(self: *Creation, model: *Model) bool {
        const remote = model.phux() orelse return false;
        var changed = false;
        _ = remote;
        for (&self.pending) |*slot| {
            if (slot.* == null) continue;
            if (slot.*.?.completion != null) continue;
            if (!finishPlacement(model, &slot.*.?)) continue;
            clearLegacy(slot);
            changed = true;
        }
        return changed;
    }

    /// A newer explicit selection supersedes delayed focus even when the
    /// current pane has not moved. Internal admission continuations do not.
    pub fn supersedeFocus(self: *Creation) void {
        for (&self.pending) |*slot| {
            if (slot.*) |*entry| entry.may_focus = false;
        }
    }

    /// Successful admission may supersede older focus without canceling itself.
    pub fn supersedeFocusExcept(self: *Creation, ref: TerminalRef) void {
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.terminal) |terminal| if (terminal.eql(ref)) continue;
            entry.may_focus = false;
        }
    }

    /// A newly accepted spawn has a command ID before it has terminal identity.
    pub fn supersedeFocusExceptCommand(self: *Creation, command_id: u64) void {
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.command_id == command_id) continue;
            entry.may_focus = false;
        }
    }

    /// Called by the engine's focus synchronization, including local gestures.
    /// Once the user leaves, returning before completion does not revive focus
    /// authority for an earlier command.
    pub fn observeFocus(self: *Creation, model: anytype) void {
        for (&self.pending) |*slot| {
            if (slot.* == null) continue;
            if (sessionWaiting(slot.*.?)) continue;
            if (!focusUnchanged(model, slot.*.?)) slot.*.?.may_focus = false;
        }
    }

    pub fn complete(self: *Creation, model: *Model, result: support.OperationResult) bool {
        const slot = self.find(result) orelse return false;
        acceptResult(model, &slot.*.?, result) catch |err| {
            finishFailure(model, &slot.*.?, failurePlacement(err), failureReason(err));
            clearLegacy(slot);
        };
        return true;
    }

    /// Teardown drains known evidence without starting a follow-up attachment.
    pub fn completeDisconnected(self: *Creation, model: *Model, result: support.OperationResult) bool {
        const slot = self.find(result) orelse return false;
        acceptOperation(&slot.*.?, result) catch |err| {
            finishFailure(model, &slot.*.?, failurePlacement(err), failureReason(err));
            clearLegacy(slot);
            return true;
        };
        finishFailure(model, &slot.*.?, .unknown, .disconnected);
        clearLegacy(slot);
        return true;
    }

    fn find(self: *Creation, result: support.OperationResult) ?*?Pending {
        for (&self.pending) |*slot| {
            const entry = slot.* orelse continue;
            if (entry.completion != null or entry.accepted) continue;
            if (entry.request != result.request_id or entry.epoch != result.connection_epoch) continue;
            return slot;
        }
        return null;
    }

    pub fn disconnect(self: *Creation, model: *Model) void {
        self.disconnectExcept(model, null);
    }

    pub fn disconnectExcept(self: *Creation, model: *Model, command_id: ?u64) void {
        model.shared_mutations.completeDisconnected(model);
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.completion != null) continue;
            if (preserveSessionDisconnect(model, entry, command_id)) continue;
            const child = retireMutation(model, entry);
            finishRetirement(model, entry, child, .disconnected);
            clearLegacy(slot);
        }
    }

    /// Call after shared_workspace.apply, including an unchanged projection.
    /// A selection hint is not evidence of placement or focus.
    pub fn observeProjection(self: *Creation, model: *Model) bool {
        var changed = false;
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.completion != null) continue;
            if (sessionWaiting(entry.*)) {
                changed = self.observeSessionProjection(model, entry) or changed;
                continue;
            }
            if (entry.projected_id == null) continue;
            changed = observeProjectedEntry(model, entry) or changed;
        }
        return changed;
    }

    /// An actual shared apply failure is a terminal placement refusal, not a
    /// reason to lose already observed operation success or wait for a wake.
    pub fn projectionFailed(self: *Creation, model: *Model) bool {
        var changed = false;
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.completion != null) continue;
            if (!awaitingProjection(entry.*)) continue;
            finishFailure(model, entry, .refused, .projection_refused);
            clearLegacy(slot);
            changed = true;
        }
        return changed;
    }

    fn observeSessionProjection(self: *Creation, model: *Model, entry: *Pending) bool {
        const navigation = &entry.navigation.?;
        if (navigation.phase != .waiting_projection) return false;
        if (!navigation.projectionReady(model)) return false;
        if (entry.terminal == null) {
            recordCompletion(entry, .not_requested, if (entry.may_focus) .not_requested else .superseded, .completed);
            return true;
        }
        self.continueSessionTerminal(model, entry) catch |err| {
            finishFailure(model, entry, failurePlacement(err), failureReason(err));
        };
        return true;
    }

    fn continueSessionTerminal(self: *Creation, model: *Model, entry: *Pending) !void {
        const remote = model.phux().?;
        const ref = entry.terminal.?;
        if (remote.terminalSession(ref) != entry.session) return error.StaleTarget;
        entry.existing_member = sharedMember(model, ref);
        if (entry.existing_member) {
            captureMemberDestination(model, entry);
        } else try self.validateSessionDestination(model, entry.*);
        // Only intentional topology replacement changes this baseline. Explicit
        // user selection has already irreversibly revoked may_focus.
        entry.focus = model.focusedTerminalRef();
        entry.focus_window = model.active_window;
        entry.navigation.?.phase = .terminal;
        if (remote.presentation(ref)) |view| {
            if (view.phase == .live) {
                entry.accepted = true;
                _ = finishPlacement(model, entry);
                return;
            }
        }
        entry.request = remote.requestAttach(ref) catch return error.AttachUnavailable;
        entry.attach_request = entry.request;
    }

    fn validateSessionDestination(self: *Creation, model: *Model, entry: Pending) !void {
        if (!destinationCurrent(model, entry)) return error.StaleDestination;
        // This entry already owns a reservation. Counting it again would reject
        // the last available slot when the target projection fills capacity.
        if (!hasCapacity(model, self.count() - 1)) return error.TerminalCapacity;
        const workspace = model.wsAtConst(entry.window) orelse return error.StaleDestination;
        var reserved: usize = 0;
        for (self.pending) |slot| {
            const other = slot orelse continue;
            if (other.completion != null or other.command_id == entry.command_id) continue;
            if (other.window == entry.window and other.window_epoch == entry.window_epoch) reserved += 1;
        }
        try validateDestinationCapacity(workspace, .tab, reserved, true);
    }

    pub fn peekCompletion(self: *const Creation) ?results.Result {
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.completion) |completion| return completion;
        }
        return null;
    }

    pub fn completionFor(self: *const Creation, command_id: u64) ?results.Result {
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            const completion = entry.completion orelse continue;
            if (completion.command_id == command_id) return completion;
        }
        return null;
    }

    pub fn ackCompletion(self: *Creation, command_id: u64) bool {
        for (&self.pending) |*slot| {
            const entry = slot.* orelse continue;
            const completion = entry.completion orelse continue;
            if (completion.command_id != command_id) continue;
            slot.* = null;
            return true;
        }
        return false;
    }
};

fn sessionWaiting(entry: Pending) bool {
    const navigation = entry.navigation orelse return false;
    return navigation.phase != .terminal;
}

fn awaitingProjection(entry: Pending) bool {
    if (entry.projected_id != null) return true;
    const navigation = entry.navigation orelse return false;
    return navigation.phase == .waiting_projection;
}

fn preserveSessionDisconnect(model: *Model, entry: *Pending, command_id: ?u64) bool {
    const id = command_id orelse return false;
    if (entry.command_id != id) return false;
    const navigation = if (entry.navigation) |*value| value else return false;
    const remote = model.phux() orelse return false;
    return navigation.preserveDisconnect(remote);
}

fn finishSessionObservationFailure(model: *Model, entry: *Pending, err: anyerror) void {
    const child = retireMutation(model, entry);
    if (child != null) {
        finishRetirement(model, entry, child, failureReason(err));
        return;
    }
    finishFailure(model, entry, failurePlacement(err), failureReason(err));
}

fn clearLegacy(slot: *?Pending) void {
    if (slot.*.?.command_id == null) slot.* = null;
}

fn retireMutation(model: *Model, entry: *Pending) ?shared_mutations.Completion {
    const ticket = entry.mutation_ticket orelse return null;
    const completion = model.shared_mutations.takeCreationCompletion(ticket);
    if (completion) |value| {
        captureMutationCompletion(entry, value);
    } else entry.placement_request = model.shared_mutations.creationRequest(ticket);
    model.shared_mutations.forget(ticket);
    entry.mutation_ticket = null;
    return completion;
}

fn captureMutationCompletion(entry: *Pending, completion: shared_mutations.Completion) void {
    entry.placement_request = completion.request_id;
    entry.mutation_outcome = switch (completion.outcome) {
        .confirmed => .success,
        .refused => .refused,
        .unknown_outcome => .unknown,
    };
}

fn finishRetirement(model: *Model, entry: *Pending, child: ?shared_mutations.Completion, reason: results.Reason) void {
    if (child) |completion| {
        if (completion.outcome == .refused) {
            finishFailure(model, entry, .refused, completion.reason);
            return;
        }
    }
    finishFailure(model, entry, .unknown, reason);
}

fn recordCompletion(entry: *Pending, placement: results.Placement, focus: results.Focus, reason: results.Reason) void {
    const command_id = entry.command_id orelse return;
    entry.completion = .{
        .command_id = command_id,
        .request_id = entry.operation_request,
        .connection_epoch = if (entry.navigation != null) 0 else entry.epoch,
        .target_session_id = if (entry.navigation) |navigation| navigation.target else 0,
        .terminal_ref = entry.terminal,
        .placement_request_id = entry.placement_request,
        .placement_connection_epoch = if (entry.mutation_identity != 0) entry.epoch else 0,
        .mutation_ticket = entry.mutation_identity,
        .mutation_outcome = entry.mutation_outcome,
        .attach_request_id = entry.attach_request,
        .attach_connection_epoch = if (entry.attach_request != 0) entry.epoch else 0,
        .error_domain = entry.error_domain,
        .error_code = entry.error_code,
        .destination_window = entry.placement_window orelse entry.window,
        .destination_window_epoch = if (entry.placement_window != null) entry.placement_window_epoch else entry.window_epoch,
        .shared_window_id = entry.projected_id orelse if (isSplit(entry.kind)) entry.shared_window else null,
        .operation = entry.operation orelse .unknown,
        .placement = placement,
        .focus = focus,
        .reason = reason,
    };
}

fn finishFailure(model: anytype, entry: *Pending, placement: results.Placement, reason: results.Reason) void {
    if (comptime @TypeOf(model) == *Model) cleanupAdoptedSpawn(model, entry, placement);
    // Retiring a provisional native window says nothing about durable execution.
    if (placement == .unknown) retireEmptyDestination(model, entry.*) else refusePlacement(model, entry.*);
    recordCompletion(entry, placement, if (entry.may_focus) .not_requested else .superseded, reason);
}

fn validateAdoptedSpawn(model: *Model, remote: *support.PhuxProvider, result: support.OperationResult) !TerminalRef {
    if (model.phux() != remote) return error.StaleContext;
    if (remote.state() != .attached) return error.NotReady;
    if (result.connection_epoch != remote.connectionEpoch()) return error.StaleContext;
    if (result.kind != .spawn) return error.InvalidOperation;
    try requireOperationSuccess(result, true);
    const ref = result.terminal_ref orelse return error.MissingIdentity;
    if (result.instance == null) return error.MissingIdentity;
    if (ref.provider_id != remote.providerId()) return error.IdentityMismatch;
    try requireWorkspace(model);
    return ref;
}

/// A sent mutation can still win after local projection or destination failure.
/// Only a known refusal permits cleanup then; unknown outcomes never replay.
fn cleanupAdoptedSpawn(model: *Model, entry: *Pending, placement: results.Placement) void {
    if (comptime !support.phux_enabled) return;
    const instance = entry.instance orelse return;
    entry.instance = null;
    if (placement == .unknown) return;
    if (entry.mutation_identity != 0 and entry.mutation_outcome != .refused) return;
    const remote = model.phux() orelse return;
    if (!adoptedCleanupCurrent(model, entry.*, remote)) return;
    _ = remote.requestKillIf(entry.terminal.?, instance) catch return;
}

fn adoptedCleanupCurrent(model: *Model, entry: Pending, remote: *support.PhuxProvider) bool {
    if (!entryContextCurrent(model, entry, remote.connectionEpoch())) return false;
    // Another writer may have admitted the terminal independently of this add.
    return !sharedMember(model, entry.terminal.?);
}

fn failurePlacement(err: anyerror) results.Placement {
    return switch (err) {
        error.StaleContext, error.NoProvider, error.OperationUnknown, error.AttachUnknown => .unknown,
        error.StaleDestination => .destination_lost,
        else => .refused,
    };
}

fn failureReason(err: anyerror) results.Reason {
    return switch (err) {
        error.StaleContext, error.NoProvider => .context_changed,
        error.StaleDestination => .destination_lost,
        error.WorkspaceUnavailable => .workspace_unavailable,
        error.OperationCapacity => .operation_capacity,
        error.StaleTarget => .stale_target,
        error.SessionMismatch => .identity_mismatch,
        else => operationFailureReason(err),
    };
}

fn operationFailureReason(err: anyerror) results.Reason {
    return switch (err) {
        error.OperationFailed => .operation_refused,
        error.OperationUnknown => .operation_unknown,
        error.AttachRefused => .attach_refused,
        error.AttachUnknown => .attach_unknown,
        error.AttachUnavailable => .attach_unavailable,
        error.MissingIdentity => .missing_identity,
        error.IdentityMismatch => .identity_mismatch,
        else => .mutation_refused,
    };
}

fn retireEmptyDestination(model: anytype, entry: Pending) void {
    if (entry.kind != .window) return;
    if (model.window_epochs[entry.window] != entry.window_epoch) return;
    const workspace = model.wsAt(entry.window) orelse return;
    if (workspace.tab_count != 0) return;
    model.closeWindow(entry.window);
}

fn prepareDestination(model: *Model, kind: Kind, reserved: usize, requires_capacity: bool) !Pending {
    const window = if (kind == .window) model.freeWindowIndex() orelse return error.WindowCapacity else model.active_window;
    if (model.window_epochs[window] == @import("std").math.maxInt(u64)) return error.StaleDestination;
    const workspace = model.wsAt(model.active_window) orelse return error.InvalidDestination;
    try validateDestinationCapacity(workspace, kind, reserved, requires_capacity);
    const entry: Pending = .{
        .window = window,
        .window_epoch = model.window_epochs[window],
        .kind = kind,
        .origin = model.focusedTerminalRef(),
        .session = model.shared_workspace.session,
        .shared_window = workspace.shared_ids[workspace.selected_tab],
        .focus = if (kind == .window) null else model.focusedTerminalRef(),
    };
    return entry;
}

fn allocateDestination(model: *Model, remote: *const support.PhuxProvider, entry: Pending) !void {
    // Result storage is already reserved; no fallible allocation follows spawn.
    if (entry.kind != .window) return;
    _ = model.openWindow(entry.window) orelse return error.WindowCapacity;
    // Bound before it is selected, as a peer's New Window is (peer_edits):
    // until its tab lands, New Session from this empty window must reach the
    // coordinator creating it, not the canonical local default. The errdefer
    // that retires it closes the window, and closing clears the binding.
    model.bindWindowAttachment(entry.window, remote.context_id);
}

fn validateDestinationCapacity(workspace: *const model_module.Workspace, kind: Kind, reserved: usize, requires_capacity: bool) !void {
    if (isSplit(kind)) try validateSplit(workspace, reserved);
    if (!requires_capacity or kind != .tab) return;
    if (workspace.tab_count + reserved >= model_module.max_tabs) return error.TabCapacity;
}

fn spawnViewport(remote: anytype, owner: ?TerminalRef) contract.Viewport {
    if (owner) |ref| return remote.lastViewport(ref) orelse remote.attach_viewport;
    return remote.attach_viewport;
}

fn requireWorkspace(model: *Model) !void {
    const remote = model.phux().?;
    const snapshot = remote.workspaceSnapshot();
    if (model.shared_workspace.session == 0) return error.WorkspaceUnavailable;
    if (snapshot.session_id != model.shared_workspace.session) return error.StaleContext;
    if (model.shared_workspace.epoch != remote.connectionEpoch()) return error.StaleContext;
    if (snapshot.state == .unavailable or snapshot.state == .last_good_error) return error.WorkspaceUnavailable;
    if (snapshot.revision != model.shared_workspace.revision) return error.StaleDestination;
}

fn acceptResult(model: *Model, entry: *Pending, result: support.OperationResult) !void {
    try acceptOperation(entry, result);
    const remote = model.phux() orelse return error.NoProvider;
    if (!entryContextCurrent(model, entry.*, remote.connectionEpoch())) return error.StaleContext;
    const ref = entry.terminal.?;
    if (result.kind == .spawn and ref.terminal_id.phux.kind == 1) {
        entry.request = remote.requestAttach(ref) catch return error.AttachUnavailable;
        entry.attach_request = entry.request;
    } else entry.accepted = true;
}

fn acceptOperation(entry: *Pending, result: support.OperationResult) !void {
    entry.error_domain = @intFromEnum(result.error_domain);
    entry.error_code = result.error_code;
    const primary = entry.operation == null;
    if (primary) entry.operation = switch (result.status) {
        .success => .success,
        .refused => .refused,
        .unknown_outcome => .unknown,
    };
    try requireOperationSuccess(result, primary);
    try acceptIdentity(entry, result.terminal_ref);
}

fn requireOperationSuccess(result: support.OperationResult, primary: bool) !void {
    switch (result.status) {
        .success => {},
        .refused => return if (primary) error.OperationFailed else error.AttachRefused,
        .unknown_outcome => return if (primary) error.OperationUnknown else error.AttachUnknown,
    }
}

fn acceptIdentity(entry: *Pending, terminal: ?TerminalRef) !void {
    const ref = terminal orelse return error.MissingIdentity;
    if (entry.terminal) |expected| if (!expected.eql(ref)) return error.IdentityMismatch;
    entry.terminal = ref;
}

fn finishPlacement(model: *Model, entry: *Pending) bool {
    if (sessionWaiting(entry.*)) return false;
    const remote = model.phux() orelse return false;
    if (!entryContextCurrent(model, entry.*, remote.connectionEpoch())) {
        const child = retireMutation(model, entry);
        finishRetirement(model, entry, child, .context_changed);
        return true;
    }
    if (entry.projected_id != null) return false;
    if (!focusUnchanged(model, entry.*)) entry.may_focus = false;
    if (entry.mutation_ticket) |ticket| return finishMutationTracked(model, entry, ticket);
    return finishPublication(model, entry);
}

fn finishPublication(model: *Model, entry: *Pending) bool {
    const remote = model.phux().?;
    if (!entry.accepted) return false;
    const ref = entry.terminal orelse return false;
    if (!remote.terminalKnown(ref)) {
        finishFailure(model, entry, .refused, .terminal_lost);
        return true;
    }
    const view = remote.presentation(ref) orelse return false;
    if (publicationFailure(view.phase)) |reason| {
        finishFailure(model, entry, .refused, reason);
        return true;
    }
    if (view.phase != .live) return false;
    return submitShared(model, entry, ref) catch |err| {
        finishFailure(model, entry, failurePlacement(err), failureReason(err));
        return true;
    };
}

fn entryContextCurrent(model: *Model, entry: Pending, epoch: u64) bool {
    if (entry.spawn_source) |source| {
        const remote = model.phux() orelse return false;
        if (!source.matches(remote)) return false;
    }
    if (entry.navigation) |navigation| {
        const remote = model.phux() orelse return false;
        if (!navigation.contextCurrent(remote)) return false;
    }
    return contextCurrent(model, entry, epoch);
}

fn contextCurrent(model: anytype, entry: Pending, epoch: u64) bool {
    if (entry.epoch != epoch) return false;
    if (entry.session == 0) return false;
    return entry.session == model.shared_workspace.session;
}

fn destinationCurrent(model: anytype, entry: Pending) bool {
    const window = entry.placement_window orelse entry.window;
    const epoch = if (entry.placement_window != null) entry.placement_window_epoch else entry.window_epoch;
    if (!model.windowOpen(window)) return false;
    if (epoch == @import("std").math.maxInt(u64)) return false;
    return model.window_epochs[window] == epoch;
}

fn captureMemberDestination(model: *const Model, entry: *Pending) void {
    const location = model.locateTerminal(entry.terminal.?) orelse return;
    entry.placement_window = location.window;
    entry.placement_window_epoch = model.window_epochs[location.window];
}

fn focusUnchanged(model: anytype, entry: Pending) bool {
    if (model.active_window != (entry.focus_window orelse entry.window)) return false;
    const current = model.focusedTerminalRef();
    const expected = entry.focus orelse return current == null;
    return if (current) |ref| expected.eql(ref) else false;
}

fn submitShared(model: *Model, entry: *Pending, ref: TerminalRef) !bool {
    if (!destinationCurrent(model, entry.*)) return error.StaleDestination;
    const snapshot = model.phux().?.workspaceSnapshot();
    if (snapshot.session_id != entry.session) return error.StaleContext;
    if (snapshot.state == .unavailable or snapshot.state == .last_good_error) return error.WorkspaceUnavailable;
    if (entry.attach_only) {
        if (shared_mutations.terminalWindow(snapshot, ref)) |id| {
            entry.existing_member = true;
            if (entry.placement_window == null) captureMemberDestination(model, entry);
            publishSelection(model, entry.*, ref, id);
            return awaitProjection(entry, id);
        }
    }
    const mutation = try creationMutation(entry.*, ref, snapshot.revision);
    entry.mutation_ticket = try model.shared_mutations.requestCreation(model, mutation, entry.epoch);
    entry.mutation_identity = entry.mutation_ticket.?;
    _ = model.shared_mutations.pump(model);
    return false;
}

fn creationMutation(entry: Pending, ref: TerminalRef, revision: u64) !contract.workspace.Mutation {
    var mutation: contract.workspace.Mutation = .{
        .expected_revision = revision,
        .session_id = entry.session,
        .kind = .add,
        .terminal_ref = ref,
    };
    if (isSplit(entry.kind)) {
        mutation.kind = .split;
        mutation.window_id = entry.shared_window orelse return error.StaleDestination;
        mutation.terminal_ref = entry.origin orelse return error.StaleDestination;
        mutation.new_terminal_ref = ref;
        mutation.direction = if (entry.kind == .split_right) .horizontal else .vertical;
    }
    return mutation;
}

fn sharedMember(model: *Model, ref: TerminalRef) bool {
    return shared_mutations.terminalWindow(model.phux().?.workspaceSnapshot(), ref) != null;
}

fn finishMutation(model: anytype, entry: Pending, ticket: u64) bool {
    var tracked = entry;
    return finishMutationTracked(model, &tracked, ticket);
}

fn finishMutationTracked(model: anytype, entry: *Pending, ticket: u64) bool {
    if (!selectionSlotAvailable(model, entry.*)) return false;
    const completion = model.shared_mutations.takeCreationCompletion(ticket) orelse return false;
    entry.mutation_ticket = null;
    captureMutationCompletion(entry, completion);
    if (completion.outcome != .confirmed) {
        finishFailure(model, entry, if (completion.outcome == .unknown_outcome) .unknown else .refused, completion.reason);
        return true;
    }
    if (!destinationCurrent(model, entry.*)) {
        finishFailure(model, entry, .destination_lost, .destination_lost);
        return true;
    }
    const ref = entry.terminal.?;
    const id = confirmedDestination(model.phux().?.workspaceSnapshot(), entry.*, ref) orelse {
        // SET succeeded but another writer's value won. Keep the process in
        // discovery and reconcile that value instead of manufacturing a pane.
        finishFailure(model, entry, .refused, .competing_topology);
        return true;
    };
    publishSelection(model, entry.*, ref, id);
    return awaitProjection(entry, id);
}

fn selectionSlotAvailable(model: anytype, entry: Pending) bool {
    if (entry.existing_member or isSplit(entry.kind)) return true;
    return model.shared_workspace.placement_hint == null;
}

fn awaitProjection(entry: *Pending, id: [16]u8) bool {
    if (entry.command_id == null) return true;
    entry.projected_id = id;
    return false;
}

fn observeProjectedEntry(model: *Model, entry: *Pending) bool {
    const remote = model.phux() orelse return false;
    if (!entryContextCurrent(model, entry.*, remote.connectionEpoch())) {
        finishFailure(model, entry, .unknown, .context_changed);
        return true;
    }
    refreshSessionMemberDestination(model, entry);
    if (!destinationCurrent(model, entry.*)) {
        finishFailure(model, entry, .destination_lost, .destination_lost);
        return true;
    }
    const snapshot = remote.workspaceSnapshot();
    if (snapshot.state == .unavailable or snapshot.state == .last_good_error) return false;
    if (model.shared_workspace.epoch != entry.epoch) return false;
    if (model.shared_workspace.revision != snapshot.revision) return false;
    if (!winningProjection(snapshot, entry.*)) {
        finishFailure(model, entry, .refused, .competing_topology);
        return true;
    }
    return finishProjectedPlacement(model, entry);
}

fn refreshSessionMemberDestination(model: *Model, entry: *Pending) void {
    if (entry.navigation == null or !entry.existing_member) return;
    const ref = entry.terminal orelse return;
    const location = model.locateTerminal(ref) orelse return;
    const workspace = model.wsAtConst(location.window) orelse return;
    const id = workspace.shared_ids[location.tab] orelse return;
    entry.placement_window = location.window;
    entry.placement_window_epoch = model.window_epochs[location.window];
    entry.projected_id = id;
}

fn winningProjection(snapshot: contract.workspace.Snapshot, entry: Pending) bool {
    const ref = entry.terminal orelse return false;
    const id = if (entry.existing_member)
        shared_mutations.terminalWindow(snapshot, ref)
    else
        confirmedDestination(snapshot, entry, ref);
    const actual = id orelse return false;
    return @import("std").mem.eql(u8, &actual, &entry.projected_id.?);
}

fn finishProjectedPlacement(model: *Model, entry: *Pending) bool {
    const ref = entry.terminal.?;
    const remote = model.phux().?;
    if (!remote.terminalKnown(ref)) {
        finishFailure(model, entry, .refused, .terminal_lost);
        return true;
    }
    const view = remote.presentation(ref) orelse return false;
    if (publicationFailure(view.phase)) |reason| {
        finishFailure(model, entry, .refused, reason);
        return true;
    }
    if (view.phase != .live) return false;
    if (!exactProjectedDestination(model, entry.*)) {
        finishFailure(model, entry, .refused, .projection_refused);
        return true;
    }
    const focus: results.Focus = if (!entry.may_focus) .superseded else projectedFocus(model, ref);
    recordCompletion(entry, .placed, focus, .completed);
    return true;
}

fn publicationFailure(phase: contract.Phase) ?results.Reason {
    return switch (phase) {
        .tombstoned, .ended => .terminal_lost,
        .failed => .publication_failed,
        else => null,
    };
}

fn exactProjectedDestination(model: *const Model, entry: Pending) bool {
    const location = model.locateTerminal(entry.terminal.?) orelse return false;
    if (location.window != (entry.placement_window orelse entry.window)) return false;
    const workspace = model.wsAtConst(location.window) orelse return false;
    const id = workspace.shared_ids[location.tab] orelse return false;
    return @import("std").mem.eql(u8, &id, &entry.projected_id.?);
}

fn projectedFocus(model: *const Model, ref: TerminalRef) results.Focus {
    const focused = model.focusedTerminalRef() orelse return .not_requested;
    return if (focused.eql(ref)) .focused else .not_requested;
}

fn confirmedDestination(snapshot: contract.workspace.Snapshot, entry: Pending, ref: TerminalRef) ?[16]u8 {
    const id = shared_mutations.terminalWindow(snapshot, ref) orelse return null;
    if (isSplit(entry.kind)) {
        return if (confirmedSplit(snapshot, entry, id, ref)) id else null;
    }
    const window = shared_mutations.findWindow(snapshot, id) orelse return null;
    if (window.root >= snapshot.nodes.len) return null;
    // An add creates one shared window. A winning concurrent arrangement that
    // moved this terminal into a group must not move that entire group locally.
    if (snapshot.nodes[window.root].kind != .leaf) return null;
    return id;
}

fn confirmedSplit(snapshot: contract.workspace.Snapshot, entry: Pending, id: [16]u8, ref: TerminalRef) bool {
    const expected = entry.shared_window orelse return false;
    if (!@import("std").mem.eql(u8, &expected, &id)) return false;
    const origin = entry.origin orelse return false;
    const window = shared_mutations.findWindow(snapshot, id) orelse return false;
    return containsSplit(snapshot, window.root, origin, ref, entry.kind, 0);
}

fn containsSplit(snapshot: contract.workspace.Snapshot, index: u32, origin: TerminalRef, ref: TerminalRef, kind: Kind, depth: usize) bool {
    if (index >= snapshot.nodes.len or depth > 64) return false;
    const node = snapshot.nodes[index];
    if (node.kind == .leaf) return false;
    if (exactSplit(snapshot, node, origin, ref, kind)) return true;
    return containsSplit(snapshot, node.first, origin, ref, kind, depth + 1) or
        containsSplit(snapshot, node.second, origin, ref, kind, depth + 1);
}

fn exactSplit(snapshot: contract.workspace.Snapshot, node: contract.workspace.Node, origin: TerminalRef, ref: TerminalRef, kind: Kind) bool {
    const direction: @TypeOf(node.kind) = if (kind == .split_right) .horizontal else .vertical;
    return node.kind == direction and leafIs(snapshot, node.first, origin) and leafIs(snapshot, node.second, ref);
}

fn leafIs(snapshot: contract.workspace.Snapshot, index: u32, ref: TerminalRef) bool {
    if (index >= snapshot.nodes.len) return false;
    const node = snapshot.nodes[index];
    if (node.kind != .leaf) return false;
    const actual = node.terminal_ref orelse return false;
    return actual.eql(ref);
}

fn publishSelection(model: anytype, entry: Pending, ref: TerminalRef, id: [16]u8) void {
    if (!destinationCurrent(model, entry)) return;
    if (!entry.existing_member and !isSplit(entry.kind)) {
        model.shared_workspace.placement_hint = .{ .shared_id = id, .window = entry.window, .window_epoch = entry.window_epoch };
    }
    if (entry.may_focus and focusUnchanged(model, entry)) model.shared_workspace.desired_terminal = ref;
}

fn refusePlacement(model: anytype, entry: Pending) void {
    retireEmptyDestination(model, entry);
    model.terminal_limit_refused = true;
    model.shared_workspace.refused = true;
}

fn isSplit(kind: Kind) bool {
    return kind == .split_right or kind == .split_down;
}

fn sharesCapacity(entry: Pending, workspace: *const model_module.Workspace, kind: Kind) bool {
    if (!isSplit(kind)) return !isSplit(entry.kind);
    if (!isSplit(entry.kind)) return false;
    const target = entry.shared_window orelse return false;
    const selected = workspace.shared_ids[workspace.selected_tab] orelse return false;
    return @import("std").mem.eql(u8, &target, &selected);
}

fn hasCapacity(model: *const Model, reserved: usize) bool {
    var count = reserved;
    for (0..model_module.max_windows) |window| {
        if (!model.windowOpen(window)) continue;
        const workspace = model.wsAtConst(window) orelse continue;
        for (workspace.tabs[0..workspace.tab_count]) |tree| count += tree.paneCount();
    }
    return count < @import("topology.zig").max_terminals;
}

/// Whether a creation refusal is a capacity limit (terminals, panes, tabs,
/// outstanding operations) rather than a refused destination or owner.
pub fn isCapacityError(err: anyerror) bool {
    return err == error.TerminalCapacity or err == error.PaneCapacity or
        err == error.TabCapacity or err == error.OperationCapacity;
}

/// New Window with another coordinator's pane focused opens on the active
/// coordinator without an owner. New Tab and splits of that pane go to that
/// coordinator (Engine.peerCreate, peer_edits) before reaching here; a split
/// that reaches here anyway is refused: its new pane would belong to the
/// other coordinator's tab.
fn focusedOwner(model: *const Model, kind: Kind) !?TerminalRef {
    const ref = model.focusedTerminalRef() orelse return null;
    if (support.providerKind(ref) == .phux and !model.activeOwnsRef(ref)) {
        if (isSplit(kind)) return error.ForeignCoordinator;
        return null;
    }
    return spawnOwner(model, ref);
}

fn spawnOwner(model: *const Model, origin: ?TerminalRef) !?TerminalRef {
    const ref = origin orelse return null;
    if (support.providerKind(ref) != .phux) return error.InvalidDestination;
    _ = model.terminalOwner(ref) orelse return error.NotReady;
    return ref;
}

fn validateSplit(workspace: *const model_module.Workspace, reserved: usize) !void {
    const tree = workspace.selectedTreeConst() orelse return error.InvalidDestination;
    if (tree.focusedTerminal() == null) return error.InvalidDestination;
    if (tree.paneCount() + reserved >= layout.max_panes) return error.PaneCapacity;
}

test {
    _ = @import("session_navigation_tests.zig");
    _ = @import("durable_creation_adopt_tests.zig");
}

const ConfirmationFixture = struct {
    const ref: TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 9 } } };
    const id: [16]u8 = @splat(4);
    const windows = [_]contract.workspace.Window{.{ .id = id, .root = 0 }};
    const nodes = [_]contract.workspace.Node{.{ .kind = .leaf, .terminal_ref = ref }};
    const Queue = struct {
        outcome: ?shared_mutations.Outcome = null,
        pub fn takeCompletion(self: *@This(), _: u64) ?shared_mutations.Outcome {
            const result = self.outcome;
            self.outcome = null;
            return result;
        }
        pub fn takeCreationCompletion(self: *@This(), ticket: u64) ?shared_mutations.Completion {
            const outcome = self.takeCompletion(ticket) orelse return null;
            return .{ .outcome = outcome, .request_id = 1, .reason = .mutation_refused };
        }
    };
    shared_mutations: Queue = .{},
    shared_workspace: struct {
        session: u32 = 7,
        refused: bool = false,
        desired_terminal: ?TerminalRef = null,
        placement_hint: ?struct { shared_id: [16]u8, window: usize, window_epoch: u64 } = null,
    } = .{},
    terminal_limit_refused: bool = false,
    window_epochs: [2]u64 = .{ 0, 5 },
    active_window: usize = 1,
    focus: ?TerminalRef = null,
    opened: bool = true,
    snapshot: contract.workspace.Snapshot = .{ .session_id = 7, .state = .authoritative, .windows = &windows, .nodes = &nodes },
    workspace: struct { tab_count: usize = 0 } = .{},

    fn entry() Pending {
        return .{ .epoch = 3, .session = 7, .window = 1, .window_epoch = 5, .kind = .window, .origin = null, .terminal = ref, .accepted = true };
    }
    pub fn phux(self: *@This()) ?*@This() {
        return self;
    }
    pub fn workspaceSnapshot(self: *@This()) contract.workspace.Snapshot {
        return self.snapshot;
    }
    pub fn windowOpen(self: *@This(), _: usize) bool {
        return self.opened;
    }
    pub fn focusedTerminalRef(self: *@This()) ?TerminalRef {
        return self.focus;
    }
    pub fn wsAt(self: *@This(), _: usize) ?@TypeOf(&self.workspace) {
        return &self.workspace;
    }
    pub fn closeWindow(self: *@This(), _: usize) void {
        self.opened = false;
    }
};

test "shared creation publishes destination only after winning confirmation" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    const entry = ConfirmationFixture.entry();
    try testing.expect(!finishMutation(&model, entry, 1));
    try testing.expect(model.shared_workspace.desired_terminal == null);
    try testing.expect(model.shared_workspace.placement_hint == null);
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(finishMutation(&model, entry, 1));
    try testing.expect(model.shared_workspace.desired_terminal.?.eql(ConfirmationFixture.ref));
    try testing.expectEqual(@as(usize, 1), model.shared_workspace.placement_hint.?.window);
    try testing.expectEqual(ConfirmationFixture.id, model.shared_workspace.placement_hint.?.shared_id);
}

test "confirmed creation cannot steal changed focus or acquire a reused native destination" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    const entry = ConfirmationFixture.entry();
    model.active_window = 0;
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(finishMutation(&model, entry, 1));
    try testing.expect(model.shared_workspace.desired_terminal == null);
    try testing.expectEqual(@as(usize, 1), model.shared_workspace.placement_hint.?.window);
    model.shared_workspace.placement_hint = null;
    model.window_epochs[1] += 1;
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(finishMutation(&model, entry, 2));
    try testing.expect(model.shared_workspace.placement_hint == null);
    try testing.expect(model.opened);
    try testing.expect(model.shared_workspace.refused);
    try testing.expect(!contextCurrent(&model, entry, 4));
    model.shared_workspace.session = 8;
    try testing.expect(!contextCurrent(&model, entry, 3));
}

test "creation focus suppression survives returning to the original focus" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    var creation: Creation = .{};
    creation.pending[0] = ConfirmationFixture.entry();
    model.active_window = 0;
    creation.observeFocus(&model);
    model.active_window = 1;
    creation.observeFocus(&model);
    const entry = creation.pending[0].?;
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(focusUnchanged(&model, entry));
    try testing.expect(finishMutation(&model, entry, 1));
    try testing.expect(model.shared_workspace.desired_terminal == null);
    try testing.expect(model.shared_workspace.placement_hint != null);
}

test "superseding selection preserves newest creation while both capture the same focus" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    var creation: Creation = .{};
    model.focus = .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 7 } } };
    creation.pending[0] = ConfirmationFixture.entry();
    creation.pending[0].?.focus = model.focus;
    const newer: TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 10 } } };
    // Selecting another catalog entry does not move focus until confirmation.
    // An observer of focus alone therefore cannot supersede the older intent.
    creation.supersedeFocus();
    creation.pending[1] = ConfirmationFixture.entry();
    creation.pending[1].?.focus = model.focus;
    creation.pending[1].?.terminal = newer;
    model.shared_workspace.desired_terminal = newer;
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(finishMutation(&model, creation.pending[0].?, 1));
    try testing.expect(model.shared_workspace.desired_terminal.?.eql(newer));
    try testing.expect(model.shared_workspace.placement_hint != null);
    const nodes = [_]contract.workspace.Node{.{ .kind = .leaf, .terminal_ref = newer }};
    model.snapshot.nodes = &nodes;
    model.shared_mutations.outcome = .confirmed;
    // The first singleton's hint must reach projection before another replaces it.
    try testing.expect(!finishMutation(&model, creation.pending[1].?, 2));
    model.shared_workspace.placement_hint = null;
    try testing.expect(finishMutation(&model, creation.pending[1].?, 2));
    try testing.expect(model.shared_workspace.desired_terminal.?.eql(newer));
}

test "superseding selection of current focus cannot resurrect a delayed creation" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    var creation: Creation = .{};
    model.focus = .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 7 } } };
    creation.pending[0] = ConfirmationFixture.entry();
    creation.pending[0].?.focus = model.focus;
    creation.supersedeFocus();
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(finishMutation(&model, creation.pending[0].?, 1));
    try testing.expect(model.shared_workspace.desired_terminal == null);
    try testing.expect(model.shared_workspace.placement_hint != null);
}

test "losing topology confirmation leaves terminal available without speculative placement" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    model.snapshot.windows = &.{};
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(finishMutation(&model, ConfirmationFixture.entry(), 1));
    try testing.expect(model.shared_workspace.desired_terminal == null);
    try testing.expect(model.shared_workspace.placement_hint == null);
    try testing.expect(model.shared_workspace.refused);
    try testing.expectEqual(@as(usize, 1), model.snapshot.nodes.len);
}

test "shared spawn waits for acceptance live publication refresh and winning split" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const testing = @import("std").testing;
    const fixture = support.PhuxProvider.test_support;
    const engine = try @import("durable_creation_tests.zig").start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    const id = model.ws().shared_ids[0].?;
    try engine.creation.request(model, .split_right);
    try testing.expectEqual(@as(usize, 1), model.ws().tabs[0].paneCount());
    try fixture.stageFixture(remote.bridge, "spawn-local.bin");
    _ = try remote.drainReadiness();
    while (remote.takeOperationResult()) |result| _ = engine.creation.complete(model, result);
    _ = engine.creation.pump(model);
    try testing.expectEqual(@as(usize, 1), model.ws().tabs[0].paneCount());
    try fixture.stageFixture(remote.bridge, "local-ready.bin");
    _ = try remote.drainReadiness();
    _ = engine.creation.pump(model);
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try testing.expectEqual(.pending, remote.workspaceSnapshot().status);
    try testing.expectEqual(@as(u32, 2), remote.workspaceSnapshot().request_id);
    // The canonical split snapshot starts from the renamed window. Refresh
    // that same authoritative name so this test confirms the exact mutation.
    try stageCreationConfirmation(remote.bridge, "workspace_rename_metadata.bin", 5, 3);
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_refresh_state.bin");
    _ = try remote.drainReadiness();
    _ = model.shared_mutations.pump(model);
    _ = engine.creation.pump(model);
    try testing.expectEqual(@as(u32, 3), remote.workspaceSnapshot().request_id);
    try testing.expectEqual(@as(usize, 1), model.ws().tabs[0].paneCount());
    try stageCreationConfirmation(remote.bridge, "workspace_split_metadata.bin", 8, 5);
    try stageCreationConfirmation(remote.bridge, "workspace_split_state.bin", 7, 4);
    _ = try remote.drainReadiness();
    _ = model.shared_mutations.pump(model);
    try testing.expect(engine.creation.pump(model));
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    // Even confirmation publishes only intent. The provider projection is the
    // sole writer of the pane tree.
    try testing.expectEqual(@as(usize, 1), model.ws().tabs[0].paneCount());
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    try testing.expectEqual(id, model.ws().shared_ids[0].?);
    try testing.expectEqual(@as(usize, 2), model.ws().tabs[0].paneCount());
    try testing.expectEqual(@as(u32, 8), model.focusedTerminalRef().?.terminal_id.phux.id);
    try testing.expectEqual(@as(u32, 3), remote.host.operation_ledger.last_id);
}

fn stageCreationConfirmation(bridge: anytype, name: []const u8, old_id: u32, new_id: u32) !void {
    const std = @import("std");
    const path = try std.fmt.allocPrint(std.testing.allocator, "src/providers/phux/fixtures/{s}", .{name});
    defer std.testing.allocator.free(path);
    const bytes = try std.Io.Dir.cwd().readFileAlloc(std.testing.io, path, std.testing.allocator, .limited(64 * 1024));
    defer std.testing.allocator.free(bytes);
    var encoded: [4]u8 = undefined;
    std.mem.writeInt(u32, &encoded, 0x8000_0000 + old_id, .big);
    // Reuse the canonical Rust topology reply, changing only correlation: this
    // scenario skips the fixture generator's preceding rename operation.
    const index = std.mem.indexOf(u8, bytes, &encoded) orelse return error.MissingCorrelation;
    try std.testing.expectEqual(index, std.mem.lastIndexOf(u8, bytes, &encoded).?);
    std.mem.writeInt(u32, bytes[index..][0..4], 0x8000_0000 + new_id, .big);
    try std.testing.expect(bridge.incoming.stage(bytes));
}

test "singleton placement hints serialize until the winning projection consumes each hint" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    var first = ConfirmationFixture.entry();
    first.command_id = 1;
    first.operation = .success;
    var second = first;
    second.command_id = 2;
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(!finishMutationTracked(&model, &first, 1));
    try testing.expect(first.projected_id != null);
    const hint = model.shared_workspace.placement_hint.?;
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(!finishMutationTracked(&model, &second, 2));
    try testing.expect(second.projected_id == null);
    try testing.expectEqual(.confirmed, model.shared_mutations.outcome.?);
    try testing.expectEqualDeep(hint, model.shared_workspace.placement_hint.?);
    model.shared_workspace.placement_hint = null;
    try testing.expect(!finishMutationTracked(&model, &second, 2));
    try testing.expect(second.projected_id != null);
}

test "winning split requires captured origin adjacency and direction rather than shared membership alone" {
    const testing = @import("std").testing;
    const origin = ConfirmationFixture.ref;
    const created: TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 10 } } };
    var nodes = [_]contract.workspace.Node{
        .{ .kind = .horizontal, .first = 1, .second = 2 },
        .{ .kind = .leaf, .terminal_ref = origin },
        .{ .kind = .leaf, .terminal_ref = created },
    };
    var model: ConfirmationFixture = .{};
    model.snapshot.nodes = &nodes;
    var entry = ConfirmationFixture.entry();
    entry.kind = .split_right;
    entry.origin = origin;
    entry.shared_window = ConfirmationFixture.id;
    try testing.expect(confirmedDestination(model.snapshot, entry, created) != null);
    nodes[0].kind = .vertical;
    try testing.expect(confirmedDestination(model.snapshot, entry, created) == null);
    nodes[0].kind = .horizontal;
    nodes[1].terminal_ref = created;
    nodes[2].terminal_ref = origin;
    try testing.expect(confirmedDestination(model.snapshot, entry, created) == null);
}

test "superseding older focus preserves newly admitted terminal focus" {
    const testing = @import("std").testing;
    var creation: Creation = .{};
    creation.pending[0] = ConfirmationFixture.entry();
    creation.pending[1] = ConfirmationFixture.entry();
    const newer: TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 10 } } };
    creation.pending[1].?.terminal = newer;
    creation.supersedeFocusExcept(newer);
    try testing.expect(!creation.pending[0].?.may_focus);
    try testing.expect(creation.pending[1].?.may_focus);
}

test "command-qualified focus supersession preserves a new spawn without terminal identity" {
    const testing = @import("std").testing;
    var creation: Creation = .{};
    creation.pending[0] = ConfirmationFixture.entry();
    creation.pending[0].?.command_id = 1;
    creation.pending[0].?.terminal = null;
    creation.pending[1] = creation.pending[0];
    creation.pending[1].?.command_id = 2;
    creation.supersedeFocusExceptCommand(2);
    try testing.expect(!creation.pending[0].?.may_focus);
    try testing.expect(creation.pending[1].?.may_focus);
}
