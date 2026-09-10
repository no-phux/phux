//! Session handoff evidence embedded in Creation.Pending. This is a value, not
//! a second command queue: the original entry owns admission and retirement.
const std = @import("std");

pub const Navigation = struct {
    pub const Phase = enum { waiting_old_close, waiting_attachment, waiting_projection, terminal };
    // Admission policy: retain exact incarnation bytes in bounded result storage.
    // Longer server identities are refused before any session/provider mutation.
    pub const max_server_bytes = 4096;

    target: u32,
    provider_context: u64,
    host_context: u64,
    old_epoch: u64,
    replacement_epoch: ?u64 = null,
    server: [max_server_bytes]u8 = undefined,
    server_len: usize,
    phase: Phase = .waiting_old_close,

    pub fn capture(remote: anytype, target: u32) !Navigation {
        if (target == 0) return error.InvalidIdentity;
        const server = remote.serverId() orelse return error.MissingServerIdentity;
        if (server.len > max_server_bytes) return error.ServerIdentityCapacity;
        var value: Navigation = .{
            .target = target,
            .provider_context = remote.context_id,
            .host_context = remote.host.context_id,
            .old_epoch = remote.connectionEpoch(),
            .server_len = server.len,
        };
        @memcpy(value.server[0..server.len], server);
        return value;
    }

    pub fn sameLifetime(self: *const Navigation, remote: anytype) bool {
        return self.provider_context == remote.context_id and self.host_context == remote.host.context_id;
    }

    pub fn preserveDisconnect(self: *Navigation, remote: anytype) bool {
        if (self.phase != .waiting_old_close) return false;
        if (!self.sameLifetime(remote)) return false;
        if (remote.connectionEpoch() != self.old_epoch) return false;
        return true;
    }

    /// Bind once, after the replacement host has actually allocated its epoch.
    pub fn bind(self: *Navigation, remote: anytype) !void {
        if (self.phase != .waiting_old_close) return error.SessionAlreadyBound;
        if (!self.sameLifetime(remote)) return error.StaleContext;
        const epoch = remote.connectionEpoch();
        if (epoch == 0 or epoch == self.old_epoch) return error.StaleContext;
        self.replacement_epoch = epoch;
        self.phase = .waiting_attachment;
    }

    pub fn contextCurrent(self: *const Navigation, remote: anytype) bool {
        if (!self.sameLifetime(remote)) return false;
        if (self.replacement_epoch != remote.connectionEpoch()) return false;
        if (remote.session_id != self.target) return false;
        if (remote.state() != .attached) return false;
        if (remote.selectedSessionId() != self.target) return false;
        const server = remote.serverId() orelse return false;
        return std.mem.eql(u8, self.server[0..self.server_len], server);
    }

    /// Session ATTACH has no resource OperationResult. Attached identity is its
    /// authoritative success evidence; projection is a separate subsequent gate.
    pub fn observeAttachment(self: *Navigation, remote: anytype) !bool {
        if (self.phase == .waiting_old_close) return false;
        if (!self.sameLifetime(remote)) return error.StaleContext;
        if (self.replacement_epoch != remote.connectionEpoch()) return error.StaleContext;
        if (remote.session_id != self.target) return error.StaleContext;
        if (remote.serverId()) |server| {
            if (!std.mem.eql(u8, self.server[0..self.server_len], server)) return error.StaleContext;
        }
        if (remote.state() != .attached) return false;
        return self.confirmAttachment(remote);
    }

    fn confirmAttachment(self: *Navigation, remote: anytype) !bool {
        if (remote.selectedSessionId() != self.target) return error.SessionMismatch;
        if (!self.contextCurrent(remote)) return error.StaleContext;
        if (self.phase != .waiting_attachment) return false;
        self.phase = .waiting_projection;
        return true;
    }

    pub fn projectionReady(self: *const Navigation, model: anytype) bool {
        const remote = model.phux() orelse return false;
        if (!self.contextCurrent(remote)) return false;
        const snapshot = remote.workspaceSnapshot();
        if (snapshot.state == .unavailable or snapshot.state == .last_good_error) return false;
        if (snapshot.session_id != self.target) return false;
        if (model.shared_workspace.session != self.target) return false;
        if (model.shared_workspace.epoch != self.replacement_epoch) return false;
        return model.shared_workspace.revision == snapshot.revision;
    }
};
