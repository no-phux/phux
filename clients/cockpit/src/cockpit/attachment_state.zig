//! Bounded durable attachment metadata, separate from live provider readiness.
const std = @import("std");
const contract = @import("provider_contract");

pub const max_references = 32;
pub const max_endpoint_bytes = 256;
pub const max_server_id_bytes = 255;

pub fn Bytes(comptime capacity: usize) type {
    return struct {
        bytes: [capacity]u8 = @splat(0),
        len: u16 = 0,

        pub fn slice(self: *const @This()) []const u8 {
            return self.bytes[0..@min(self.len, capacity)];
        }

        pub fn init(value: []const u8) error{AttachmentContextTooLong}!@This() {
            if (value.len > capacity) return error.AttachmentContextTooLong;
            var result: @This() = .{};
            @memcpy(result.bytes[0..value.len], value);
            result.len = @intCast(value.len);
            return result;
        }
    };
}

pub const Context = struct {
    endpoint: Bytes(max_endpoint_bytes) = .{},
    server_id: Bytes(max_server_id_bytes) = .{},
    session_id: u32 = 0,

    pub fn init(endpoint: []const u8, server_id: []const u8, session_id: u32) !Context {
        return .{
            .endpoint = try .init(endpoint),
            .server_id = try .init(server_id),
            .session_id = session_id,
        };
    }

    pub fn valid(self: *const Context) bool {
        return self.endpoint.len <= max_endpoint_bytes and self.server_id.len <= max_server_id_bytes;
    }

    pub fn matches(self: *const Context, other: *const Context) bool {
        if (!self.valid() or !other.valid()) return false;
        if (self.endpoint.len == 0 or self.server_id.len == 0) return false;
        return self.session_id == other.session_id and
            std.mem.eql(u8, self.endpoint.slice(), other.endpoint.slice()) and
            std.mem.eql(u8, self.server_id.slice(), other.server_id.slice());
    }

    pub fn hash(self: *const Context, hasher: *std.hash.Wyhash) void {
        std.hash.autoHash(hasher, self.endpoint.len);
        hasher.update(self.endpoint.slice());
        std.hash.autoHash(hasher, self.server_id.len);
        hasher.update(self.server_id.slice());
        std.hash.autoHash(hasher, self.session_id);
    }
};

pub const Reference = struct {
    terminal_ref: contract.TerminalRef,
    context: Context = .{},

    pub fn valid(self: *const Reference) bool {
        // Any Phux coordinator: this Mac's (`.phux`) or a registered host's
        // (contract.phuxCoordinatorId). The endpoint in the context names it.
        if (!contract.isPhuxCoordinator(self.terminal_ref.provider_id)) return false;
        const remote = switch (self.terminal_ref.terminal_id) {
            .local => return false,
            .phux => |id| id,
        };
        if (!self.context.valid()) return false;
        return switch (remote.kind) {
            0 => remote.host_len == 0,
            1 => remote.host_len > 0 and std.unicode.utf8ValidateSlice(remote.host()),
            else => false,
        };
    }

    pub fn matches(self: *const Reference, context: *const Context) bool {
        if (!self.valid()) return false;
        // HELLO_OK proves the coordinator incarnation, never a satellite's.
        if (self.terminal_ref.terminal_id.phux.kind != 0) return false;
        return self.context.matches(context);
    }
};

pub const Table = struct {
    entries: [max_references]?Reference = @splat(null),
    count: u8 = 0,

    pub fn get(self: *const Table, index: u8) ?Reference {
        if (index >= self.count or index >= max_references) return null;
        return self.entries[index];
    }

    pub fn find(self: *const Table, ref: contract.TerminalRef) ?u8 {
        for (self.entries[0..self.count], 0..) |entry, index| {
            if (entry) |known| if (known.terminal_ref.eql(ref)) return @intCast(index);
        }
        return null;
    }

    pub fn append(self: *Table, reference: Reference) !u8 {
        if (!reference.valid()) return error.InvalidTopology;
        if (self.count >= max_references) return error.InvalidTopology;
        const index = self.count;
        self.entries[index] = reference;
        self.count += 1;
        return index;
    }

    pub fn validate(self: *const Table) !void {
        if (self.count > max_references) return error.InvalidTopology;
        for (self.entries[0..self.count], 0..) |entry, index| {
            const reference = entry orelse return error.InvalidTopology;
            if (!reference.valid()) return error.InvalidTopology;
            if (self.find(reference.terminal_ref).? != index) return error.InvalidTopology;
        }
        for (self.entries[self.count..]) |entry| if (entry != null) return error.InvalidTopology;
    }
};
