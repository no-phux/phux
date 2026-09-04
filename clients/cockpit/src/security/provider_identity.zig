//! Provider-neutral durable identity, distinct from provider kind and server incarnation.

const std = @import("std");

pub const provider_instance_id_bytes: usize = 16;

pub const ProviderInstanceIdError = error{InvalidProviderInstanceId};

pub const ProviderInstanceId = struct {
    _bytes: [provider_instance_id_bytes]u8,

    pub fn fromStorage(encoded: []const u8) ProviderInstanceIdError!ProviderInstanceId {
        if (encoded.len != provider_instance_id_bytes) return error.InvalidProviderInstanceId;
        var value: [provider_instance_id_bytes]u8 = undefined;
        @memcpy(&value, encoded);
        return .{ ._bytes = value };
    }

    pub fn fromBytes(bytes: [provider_instance_id_bytes]u8) ProviderInstanceId {
        return .{ ._bytes = bytes };
    }

    pub fn storage(id: *const ProviderInstanceId) []const u8 {
        return &id._bytes;
    }

    pub fn eql(a: ProviderInstanceId, b: ProviderInstanceId) bool {
        return std.crypto.timing_safe.eql([provider_instance_id_bytes]u8, a._bytes, b._bytes);
    }

    pub fn format(id: ProviderInstanceId, writer: *std.Io.Writer) std.Io.Writer.Error!void {
        var encoded: [provider_instance_id_bytes * 2]u8 = undefined;
        encodeHex(&encoded, &id._bytes);
        try writer.print("provider-instance:{s}", .{&encoded});
    }

    pub fn jsonStringify(id: ProviderInstanceId, stringify: anytype) !void {
        var encoded: [provider_instance_id_bytes * 2]u8 = undefined;
        encodeHex(&encoded, &id._bytes);
        try stringify.write(&encoded);
    }
};

fn encodeHex(destination: []u8, bytes: []const u8) void {
    const alphabet = "0123456789abcdef";
    for (bytes, 0..) |byte, index| {
        destination[index * 2] = alphabet[byte >> 4];
        destination[index * 2 + 1] = alphabet[byte & 0x0f];
    }
}
