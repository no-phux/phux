const std = @import("std");
const credentials = @import("credential_store");
const trust = @import("credential_trust");
const identity = @import("provider_identity");

const testing = std.testing;

const FakeBackend = struct {
    account: [64]u8 = [_]u8{0} ** 64,
    account_len: usize = 0,
    secret: [64]u8 = [_]u8{0} ** 64,
    secret_len: usize = 0,
    handle: ?credentials.CredentialHandle = null,
    present: bool = false,
    fail_after_write: bool = false,
    saw_zeroed_destination: bool = false,

    fn backend(fake: *FakeBackend) credentials.Backend {
        return .{ .context = fake, .vtable = &vtable };
    }

    fn deinit(fake: *FakeBackend) void {
        credentials.secureZero(&fake.account);
        credentials.secureZero(&fake.secret);
        fake.account_len = 0;
        fake.secret_len = 0;
        fake.present = false;
        fake.clearHandle();
    }

    fn restart(fake: *FakeBackend) void {
        fake.clearHandle();
    }

    fn clearHandle(fake: *FakeBackend) void {
        if (fake.handle) |*handle| credentials.secureZero(std.mem.asBytes(handle));
        fake.handle = null;
    }

    const vtable = credentials.Backend.VTable{
        .put = put,
        .open = open,
        .resolve = resolve,
        .replace = replace,
        .delete = delete,
    };

    fn self(context: *anyopaque) *FakeBackend {
        return @ptrCast(@alignCast(context));
    }

    fn requireHandle(fake: *FakeBackend, handle: credentials.CredentialHandle) credentials.CredentialError!void {
        const expected = fake.handle orelse return error.InvalidHandle;
        if (!expected.eql(handle)) return error.InvalidHandle;
    }

    fn put(
        context: *anyopaque,
        account: []const u8,
        secret: []const u8,
        handle: credentials.CredentialHandle,
    ) credentials.CredentialError!void {
        const fake = self(context);
        if (account.len == 0) return error.InvalidRequest;
        if (fake.present) return error.AlreadyExists;
        if (secret.len > fake.secret.len) return error.SecretTooLarge;
        if (account.len > fake.account.len) return error.InvalidRequest;
        @memcpy(fake.account[0..account.len], account);
        fake.account_len = account.len;
        @memcpy(fake.secret[0..secret.len], secret);
        fake.secret_len = secret.len;
        fake.handle = handle;
        fake.present = true;
    }

    fn open(context: *anyopaque, account: []const u8, handle: credentials.CredentialHandle) credentials.CredentialError!void {
        const fake = self(context);
        if (account.len == 0) return error.InvalidRequest;
        if (!fake.present or !std.mem.eql(u8, fake.account[0..fake.account_len], account)) return error.NotFound;
        fake.clearHandle();
        fake.handle = handle;
    }
    fn resolve(context: *anyopaque, handle: credentials.CredentialHandle, destination: []u8) credentials.CredentialError!usize {
        const fake = self(context);
        fake.saw_zeroed_destination = allZero(destination);
        try fake.requireHandle(handle);
        if (destination.len < fake.secret_len) return error.BufferTooSmall;
        @memcpy(destination[0..fake.secret_len], fake.secret[0..fake.secret_len]);
        if (fake.fail_after_write) return error.BackendFailure;
        return fake.secret_len;
    }

    fn replace(context: *anyopaque, handle: credentials.CredentialHandle, secret: []const u8) credentials.CredentialError!void {
        const fake = self(context);
        try fake.requireHandle(handle);
        if (secret.len > fake.secret.len) return error.SecretTooLarge;
        credentials.secureZero(&fake.secret);
        @memcpy(fake.secret[0..secret.len], secret);
        fake.secret_len = secret.len;
    }

    fn delete(context: *anyopaque, handle: credentials.CredentialHandle) credentials.CredentialError!void {
        const fake = self(context);
        try fake.requireHandle(handle);
        credentials.secureZero(&fake.account);
        credentials.secureZero(&fake.secret);
        fake.account_len = 0;
        fake.secret_len = 0;
        fake.present = false;
        fake.clearHandle();
    }
};

test "credential backend owns random process-local handle lifecycle" {
    var fake = FakeBackend{};
    defer fake.deinit();
    const backend = fake.backend();
    const handle = try backend.put(testing.io, "provider-one", "initial-secret");
    try testing.expect(@sizeOf(credentials.CredentialHandle) <= 64);
    try testing.expectError(error.AlreadyExists, backend.put(testing.io, "provider-one", "duplicate"));

    var other_fake = FakeBackend{};
    defer other_fake.deinit();
    const foreign = try other_fake.backend().put(testing.io, "provider-two", "other-secret");
    try testing.expect(!handle.eql(foreign));

    var output = [_]u8{0xa5} ** 32;
    try testing.expectError(error.InvalidHandle, backend.resolve(foreign, &output));
    try testing.expectEqualSlices(u8, &([_]u8{0} ** output.len), &output);

    var length = try backend.resolve(handle, &output);
    try testing.expect(fake.saw_zeroed_destination);
    try testing.expectEqualStrings("initial-secret", output[0..length]);
    try testing.expect(allZero(output[length..]));

    const oversized_replacement = [_]u8{0xa5} ** (fake.secret.len + 1);
    try testing.expectError(error.SecretTooLarge, backend.replace(handle, &oversized_replacement));
    length = try backend.resolve(handle, &output);
    try testing.expectEqualStrings("initial-secret", output[0..length]);

    try backend.replace(handle, "replacement");
    @memset(&output, 0xa5);
    length = try backend.resolve(handle, &output);
    try testing.expectEqualStrings("replacement", output[0..length]);
    try testing.expect(allZero(output[length..]));
    try testing.expect(allZero(fake.secret["replacement".len..]));

    try backend.delete(handle);
    try testing.expect(allZero(&fake.secret));
    try testing.expectError(error.InvalidHandle, backend.resolve(handle, &output));
    try testing.expectEqualSlices(u8, &([_]u8{0} ** output.len), &output);
}

test "persistent credential reopens with a fresh token after registry restart" {
    var fake = FakeBackend{};
    defer fake.deinit();
    const backend = fake.backend();
    const original = try backend.put(testing.io, "provider-one", "persistent-secret");
    fake.restart();

    var output = [_]u8{0xa5} ** 32;
    try testing.expectError(error.InvalidHandle, backend.resolve(original, &output));
    try testing.expect(allZero(&output));
    try testing.expectError(error.NotFound, backend.open(testing.io, "missing-provider"));

    const reopened = try backend.open(testing.io, "provider-one");
    try testing.expect(!original.eql(reopened));
    const length = try backend.resolve(reopened, &output);
    try testing.expectEqualStrings("persistent-secret", output[0..length]);
    try testing.expect(allZero(output[length..]));
    try testing.expectError(error.AlreadyExists, backend.put(testing.io, "provider-one", "duplicate"));
}

test "resolve clears destination before dispatch and on every failure path" {
    var fake = FakeBackend{};
    defer fake.deinit();
    const backend = fake.backend();
    const handle = try backend.put(testing.io, "provider-one", "must-not-remain");

    var too_small = [_]u8{0xa5} ** 4;
    try testing.expectError(error.BufferTooSmall, backend.resolve(handle, &too_small));
    try testing.expect(fake.saw_zeroed_destination);
    try testing.expect(allZero(&too_small));

    fake.fail_after_write = true;
    var after_write = [_]u8{0xa5} ** 32;
    try testing.expectError(error.BackendFailure, backend.resolve(handle, &after_write));
    try testing.expect(fake.saw_zeroed_destination);
    try testing.expect(allZero(&after_write));
}

test "credential handle debug and JSON forms never expose tokens or secrets" {
    const secret_canary = "credential-secret-canary";
    var fake = FakeBackend{};
    defer fake.deinit();
    const handle = try fake.backend().put(testing.io, "provider-one", secret_canary);

    var debug_storage: [64]u8 = undefined;
    var debug_writer = std.Io.Writer.fixed(&debug_storage);
    try debug_writer.print("{f}", .{handle});
    try testing.expectEqualStrings("<credential-handle>", debug_writer.buffered());
    try testing.expect(std.mem.indexOf(u8, debug_writer.buffered(), secret_canary) == null);

    var json_storage: [64]u8 = undefined;
    const json = try stringify(handle, &json_storage);
    try testing.expectEqualStrings("\"credential-handle\"", json);
    try testing.expect(std.mem.indexOf(u8, json, secret_canary) == null);
}

fn providerId() !identity.ProviderInstanceId {
    return identity.ProviderInstanceId.fromStorage(&[_]u8{
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0x76, 0x07,
        0x88, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    });
}

test "trust projection exposes only unattached state before FFI verification" {
    const projection = trust.ProviderTrustProjection.initUnattached();
    try testing.expectEqual(trust.TrustState.unattached, projection.state());
    try testing.expectEqual(trust.UnattachedReason.ffi_not_integrated, projection.reason());

    var json_storage: [128]u8 = undefined;
    const json = try stringify(projection, &json_storage);
    try testing.expect(std.mem.indexOf(u8, json, "unattached") != null);
    try testing.expect(std.mem.indexOf(u8, json, "ffi_not_integrated") != null);
}

test "authority incarnation and provider identity remain distinct inert values" {
    const authority = trust.AuthorityFingerprint.fromBytes([_]u8{0x11} ** 32);
    const incarnation = trust.ServerIncarnation.fromBytes([_]u8{0x11} ** 16);
    const provider_instance = try providerId();
    try testing.expect(authority.eql(authority));
    try testing.expect(incarnation.eql(incarnation));
    try testing.expect(provider_instance.eql(provider_instance));
    comptime {
        if (trust.AuthorityFingerprint == trust.ServerIncarnation or
            trust.ServerIncarnation == identity.ProviderInstanceId)
        {
            @compileError("trust identities must remain distinct types");
        }
    }
}

test "owned opaque grant serialization omits capability bytes" {
    const secret_canary = "credential-secret-canary";
    var grant = try trust.OpaqueCanonicalGrant.initOwned(testing.allocator, secret_canary);
    defer grant.deinit(testing.allocator);
    try testing.expectEqualStrings(secret_canary, grant.bytes());

    var json_storage: [128]u8 = undefined;
    const json = try stringify(grant, &json_storage);
    try testing.expect(std.mem.indexOf(u8, json, secret_canary) == null);
    try testing.expect(std.mem.indexOf(u8, json, "opaque_canonical_byte_count") != null);
}

fn allZero(bytes: []const u8) bool {
    for (bytes) |byte| if (byte != 0) return false;
    return true;
}

fn stringify(value: anytype, storage: []u8) ![]const u8 {
    var writer = std.Io.Writer.fixed(storage);
    try std.json.Stringify.value(value, .{}, &writer);
    return writer.buffered();
}

/// Opt-in integration helper, intentionally not a default `test`: login or CI
/// keychains may be locked or prompt interactively. A caller supplies a unique
/// account and is responsible for invoking this only with an unlocked keychain.
pub fn runMacOSKeychainIntegration(unique_account: []const u8) !void {
    const macos = @import("macos_keychain");
    var keychain = macos.MacOSKeychain{};
    defer keychain.deinit();
    const backend = keychain.backend();
    const handle = try backend.put(testing.io, unique_account, "integration-secret");
    var deleted = false;
    defer if (!deleted) backend.delete(handle) catch {};

    var output: [32]u8 = undefined;
    defer credentials.secureZero(&output);
    const length = try backend.resolve(handle, &output);
    try testing.expectEqualStrings("integration-secret", output[0..length]);
    try backend.delete(handle);
    deleted = true;
}
