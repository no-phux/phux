//! Provider trust value types. Runtime trust remains unattached until verified FFI evidence exists.

const std = @import("std");

pub const max_opaque_canonical_grant_bytes: usize = 32 * 1024;

/// No verified state is representable until the Phux FFI supplies an
/// unforgeable verifier result. Adding one is part of the later integration.
pub const TrustState = enum {
    unattached,
};

pub const UnattachedReason = enum {
    ffi_not_integrated,
};

/// SHA-256 of verified authority key material. It identifies authority, not a
/// server process. This value type alone carries no authorization.
pub const AuthorityFingerprint = struct {
    _bytes: [32]u8,

    pub fn fromBytes(bytes: [32]u8) AuthorityFingerprint {
        return .{ ._bytes = bytes };
    }

    pub fn eql(a: AuthorityFingerprint, b: AuthorityFingerprint) bool {
        return std.crypto.timing_safe.eql([32]u8, a._bytes, b._bytes);
    }

    pub fn jsonStringify(fingerprint: AuthorityFingerprint, stringify: anytype) !void {
        var encoded: ["sha256:".len + 64]u8 = undefined;
        @memcpy(encoded[0.."sha256:".len], "sha256:");
        encodeHex(encoded["sha256:".len..], &fingerprint._bytes);
        try stringify.write(&encoded);
    }
};

/// Volatile 128-bit replay identity, distinct from durable authority and from
/// Cockpit's provider instance identity. This value alone carries no trust.
pub const ServerIncarnation = struct {
    _bytes: [16]u8,

    pub fn fromBytes(bytes: [16]u8) ServerIncarnation {
        return .{ ._bytes = bytes };
    }

    pub fn eql(a: ServerIncarnation, b: ServerIncarnation) bool {
        return std.crypto.timing_safe.eql([16]u8, a._bytes, b._bytes);
    }

    pub fn jsonStringify(incarnation: ServerIncarnation, stringify: anytype) !void {
        var encoded: [32]u8 = undefined;
        encodeHex(&encoded, &incarnation._bytes);
        try stringify.write(&encoded);
    }
};

pub const GrantError = error{
    EmptyGrant,
    GrantTooLarge,
};

/// Owned canonical effective-scope bytes. The type is intentionally not
/// attached to ProviderTrustProjection and cannot authorize anything until the
/// future FFI verifier boundary exists.
pub const OpaqueCanonicalGrant = struct {
    _bytes: []u8,

    pub fn initOwned(
        allocator: std.mem.Allocator,
        canonical_bytes: []const u8,
    ) (GrantError || std.mem.Allocator.Error)!OpaqueCanonicalGrant {
        if (canonical_bytes.len == 0) return error.EmptyGrant;
        if (canonical_bytes.len > max_opaque_canonical_grant_bytes) return error.GrantTooLarge;
        return .{ ._bytes = try allocator.dupe(u8, canonical_bytes) };
    }

    pub fn deinit(grant: *OpaqueCanonicalGrant, allocator: std.mem.Allocator) void {
        std.crypto.secureZero(u8, grant._bytes);
        allocator.free(grant._bytes);
        grant.* = undefined;
    }

    pub fn bytes(grant: OpaqueCanonicalGrant) []const u8 {
        return grant._bytes;
    }

    pub fn jsonStringify(grant: OpaqueCanonicalGrant, stringify: anytype) !void {
        try stringify.beginObject();
        try stringify.objectField("opaque_canonical_byte_count");
        try stringify.write(grant._bytes.len);
        try stringify.endObject();
    }
};

/// The sole runtime projection available before FFI verification is integrated.
/// Raw authority, incarnation, grant, or caller-selected enum values cannot
/// manufacture a verified variant because no such variant exists.
pub const ProviderTrustProjection = struct {
    pub fn initUnattached() ProviderTrustProjection {
        return .{};
    }

    pub fn state(_: ProviderTrustProjection) TrustState {
        return .unattached;
    }

    pub fn reason(_: ProviderTrustProjection) UnattachedReason {
        return .ffi_not_integrated;
    }

    pub fn jsonStringify(_: ProviderTrustProjection, stringify: anytype) !void {
        try stringify.beginObject();
        try stringify.objectField("trust_state");
        try stringify.write("unattached");
        try stringify.objectField("reason");
        try stringify.write("ffi_not_integrated");
        try stringify.endObject();
    }
};

fn encodeHex(destination: []u8, bytes: []const u8) void {
    const alphabet = "0123456789abcdef";
    for (bytes, 0..) |byte, index| {
        destination[index * 2] = alphabet[byte >> 4];
        destination[index * 2 + 1] = alphabet[byte & 0x0f];
    }
}
