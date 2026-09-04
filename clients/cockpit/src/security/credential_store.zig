//! Secret storage contract. Handles are process-local backend-issued capabilities.

const std = @import("std");

pub const max_persistent_reference_bytes: usize = 1024;
const credential_token_bytes: usize = 32;

pub const CredentialError = error{
    InvalidHandle,
    ReferenceTooLarge,
    SecretTooLarge,
    BufferTooSmall,
    CapacityExceeded,
    AlreadyExists,
    NotFound,
    AccessDenied,
    InteractionRequired,
    CorruptItem,
    InvalidRequest,
    OutOfMemory,
    EntropyUnavailable,
    BackendFailure,
};

pub const CredentialHandle = struct {
    _token: [credential_token_bytes]u8,

    fn issue(io: std.Io) std.Io.RandomSecureError!CredentialHandle {
        var token: [credential_token_bytes]u8 = undefined;
        try io.randomSecure(&token);
        return .{ ._token = token };
    }

    pub fn eql(a: CredentialHandle, b: CredentialHandle) bool {
        return std.crypto.timing_safe.eql([credential_token_bytes]u8, a._token, b._token);
    }

    /// Debug output never exposes the process-local capability token.
    pub fn format(_: CredentialHandle, writer: *std.Io.Writer) std.Io.Writer.Error!void {
        try writer.writeAll("<credential-handle>");
    }

    /// Handles are process-local capabilities, not portable serialized data.
    pub fn jsonStringify(_: CredentialHandle, stringify: anytype) !void {
        try stringify.write("credential-handle");
    }
};

pub const Backend = struct {
    context: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        put: *const fn (*anyopaque, []const u8, []const u8, CredentialHandle) CredentialError!void,
        open: *const fn (*anyopaque, []const u8, CredentialHandle) CredentialError!void,
        resolve: *const fn (*anyopaque, CredentialHandle, []u8) CredentialError!usize,
        replace: *const fn (*anyopaque, CredentialHandle, []const u8) CredentialError!void,
        delete: *const fn (*anyopaque, CredentialHandle) CredentialError!void,
    };

    pub fn put(
        backend: Backend,
        io: std.Io,
        account: []const u8,
        secret: []const u8,
    ) CredentialError!CredentialHandle {
        const handle = CredentialHandle.issue(io) catch return error.EntropyUnavailable;
        try backend.vtable.put(backend.context, account, secret, handle);
        return handle;
    }

    /// Reopens a persistent credential by its non-secret account identity and
    /// issues a fresh process-local handle for this backend instance.
    pub fn open(
        backend: Backend,
        io: std.Io,
        account: []const u8,
    ) CredentialError!CredentialHandle {
        const handle = CredentialHandle.issue(io) catch return error.EntropyUnavailable;
        try backend.vtable.open(backend.context, account, handle);
        return handle;
    }

    /// The entire caller-owned destination is cleared before dispatch. Failure
    /// clears it again in case a backend wrote before returning its error, and
    /// success clears every byte beyond the returned secret length.
    pub fn resolve(backend: Backend, handle: CredentialHandle, destination: []u8) CredentialError!usize {
        secureZero(destination);
        var resolved = false;
        defer if (!resolved) secureZero(destination);
        const length = try backend.vtable.resolve(backend.context, handle, destination);
        if (length > destination.len) return error.BackendFailure;
        secureZero(destination[length..]);
        resolved = true;
        return length;
    }

    pub fn replace(backend: Backend, handle: CredentialHandle, secret: []const u8) CredentialError!void {
        return backend.vtable.replace(backend.context, handle, secret);
    }

    pub fn delete(backend: Backend, handle: CredentialHandle) CredentialError!void {
        return backend.vtable.delete(backend.context, handle);
    }
};

pub fn secureZero(bytes: []u8) void {
    std.crypto.secureZero(u8, bytes);
}
