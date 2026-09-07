//! Restore subscriptions only after matching endpoint, server, and session
//! evidence. One attempt per reference per connection; refusal stays pending.
const Model = @import("model.zig").Model;
const support = @import("phux_support.zig");
const TerminalRef = support.TerminalRef;
const Attempt = struct { ref: TerminalRef, request: u32 = 0, accepted: bool = false };

pub const Recovery = struct {
    epoch: ?u64 = null,
    session: ?u32 = null,
    attempted: [32]?Attempt = @splat(null),

    pub fn pump(self: *Recovery, model: *Model) bool {
        if (comptime !support.phux_enabled) return false;
        const remote = model.phux() orelse return false;
        if (remote.state() != .attached) return false;
        if (!self.synchronizeContext(model)) return false;
        var refs: [32]TerminalRef = undefined;
        const count = model.pendingRestoredRefs(&refs);
        var changed = false;
        for (refs[0..count]) |ref| {
            changed = self.recover(model, ref) or changed;
        }
        return changed;
    }

    fn recover(self: *Recovery, model: *Model, ref: TerminalRef) bool {
        if (!model.restoredAttachmentMatches(ref)) return false;
        if (self.find(ref)) |attempt| {
            if (!attempt.accepted) return false;
            return model.resolveRestoredAttachment(ref);
        }
        if (model.resolveRestoredAttachment(ref)) return true;
        const slot = self.reserve(ref) orelse return false;
        const remote = model.phux() orelse return false;
        const request = remote.requestAttach(ref) catch |err| {
            // Capacity rejection queued nothing and can be attempted again.
            model.terminal_limit_refused = true;
            if (err == error.OperationCapacity) self.attempted[slot] = null;
            return false;
        };
        self.attempted[slot].?.request = request;
        return false;
    }

    pub fn complete(self: *Recovery, model: *Model, result: support.OperationResult) void {
        if (result.connection_epoch != self.epoch) return;
        for (&self.attempted) |*entry| {
            const attempt = if (entry.*) |*value| value else continue;
            if (attempt.request != result.request_id) continue;
            attempt.accepted = result.status == .success;
            if (!attempt.accepted) model.terminal_limit_refused = true;
            return;
        }
    }

    fn synchronizeContext(self: *Recovery, model: *Model) bool {
        const remote = model.phux() orelse return false;
        const session = remote.selectedSessionId() orelse return false;
        if (self.epoch == remote.connectionEpoch() and self.session == session) return true;
        const server = remote.serverId() orelse return false;
        const endpoint = switch (remote.endpointDescriptor()) {
            .unix => |path| path,
            else => return false,
        };
        model.setAttachmentContext(endpoint, server, session) catch return false;
        self.epoch = remote.connectionEpoch();
        self.session = session;
        @memset(&self.attempted, null);
        return true;
    }

    fn find(self: *const Recovery, ref: TerminalRef) ?Attempt {
        for (self.attempted) |entry| if (entry) |known| if (known.ref.eql(ref)) return known;
        return null;
    }

    fn reserve(self: *Recovery, ref: TerminalRef) ?usize {
        for (&self.attempted, 0..) |*entry, index| {
            if (entry.* != null) continue;
            entry.* = .{ .ref = ref };
            return index;
        }
        return null;
    }

    pub fn disconnect(self: *Recovery, model: *Model) void {
        model.rejectAttachmentContext();
        self.epoch = null;
        self.session = null;
        @memset(&self.attempted, null);
    }
};
