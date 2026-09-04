//! Security.framework generic-password credential backend for macOS.

const std = @import("std");

const store = @import("credential_store");

// Keep this narrow rather than @cImport-ing Security.h: Xcode 26's transitive
// Mach/XPC headers currently fail Zig 0.16 translation on unrelated structs.
const c = struct {
    const CFTypeRef = ?*const anyopaque;
    const CFStringRef = ?*const anyopaque;
    const CFDataRef = ?*const anyopaque;
    const CFArrayRef = ?*const anyopaque;
    const CFDictionaryRef = ?*const anyopaque;
    const CFAllocatorRef = ?*const anyopaque;
    const CFIndex = isize;
    const CFStringEncoding = u32;
    const OSStatus = i32;

    const CFDictionaryKeyCallBacks = extern struct {
        version: CFIndex,
        retain: ?*const anyopaque,
        release: ?*const anyopaque,
        copy_description: ?*const anyopaque,
        equal: ?*const anyopaque,
        hash: ?*const anyopaque,
    };
    const CFDictionaryValueCallBacks = extern struct {
        version: CFIndex,
        retain: ?*const anyopaque,
        release: ?*const anyopaque,
        copy_description: ?*const anyopaque,
        equal: ?*const anyopaque,
    };
    const CFArrayCallBacks = extern struct {
        version: CFIndex,
        retain: ?*const anyopaque,
        release: ?*const anyopaque,
        copy_description: ?*const anyopaque,
        equal: ?*const anyopaque,
    };

    extern var kCFAllocatorDefault: CFAllocatorRef;
    extern var kCFAllocatorNull: CFAllocatorRef;
    extern var kCFBooleanTrue: CFTypeRef;
    extern var kCFTypeArrayCallBacks: CFArrayCallBacks;
    extern var kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    extern var kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    extern var kSecClass: CFStringRef;
    extern var kSecClassGenericPassword: CFStringRef;
    extern var kSecAttrService: CFStringRef;
    extern var kSecAttrAccount: CFStringRef;
    extern var kSecAttrAccessible: CFStringRef;
    extern var kSecAttrAccessibleWhenUnlockedThisDeviceOnly: CFStringRef;
    extern var kSecValueData: CFStringRef;
    extern var kSecValuePersistentRef: CFStringRef;
    extern var kSecReturnPersistentRef: CFStringRef;
    extern var kSecReturnData: CFStringRef;
    extern var kSecMatchLimit: CFStringRef;
    extern var kSecMatchLimitOne: CFStringRef;
    extern var kSecMatchItemList: CFStringRef;
    extern var kSecUseDataProtectionKeychain: CFStringRef;

    extern fn CFRelease(value: CFTypeRef) void;
    extern fn CFStringCreateWithBytes(CFAllocatorRef, [*]const u8, CFIndex, CFStringEncoding, u8) CFStringRef;
    extern fn CFDataCreate(CFAllocatorRef, [*]const u8, CFIndex) CFDataRef;
    extern fn CFDataCreateWithBytesNoCopy(CFAllocatorRef, [*]const u8, CFIndex, CFAllocatorRef) CFDataRef;
    extern fn CFDataGetLength(CFDataRef) CFIndex;
    extern fn CFDataGetBytePtr(CFDataRef) [*]const u8;
    extern fn CFArrayCreate(
        CFAllocatorRef,
        [*]const ?*const anyopaque,
        CFIndex,
        *const CFArrayCallBacks,
    ) CFArrayRef;
    extern fn CFDictionaryCreate(
        CFAllocatorRef,
        [*]const ?*const anyopaque,
        [*]const ?*const anyopaque,
        CFIndex,
        *const CFDictionaryKeyCallBacks,
        *const CFDictionaryValueCallBacks,
    ) CFDictionaryRef;
    extern fn SecItemAdd(CFDictionaryRef, *CFTypeRef) OSStatus;
    extern fn SecItemCopyMatching(CFDictionaryRef, *CFTypeRef) OSStatus;
    extern fn SecItemUpdate(CFDictionaryRef, CFDictionaryRef) OSStatus;
    extern fn SecItemDelete(CFDictionaryRef) OSStatus;

    const kCFStringEncodingUTF8: CFStringEncoding = 0x08000100;
    const errSecSuccess: OSStatus = 0;
    const errSecParam: OSStatus = -50;
    const errSecAllocate: OSStatus = -108;
    const errSecAuthFailed: OSStatus = -25293;
    const errSecDuplicateItem: OSStatus = -25299;
    const errSecItemNotFound: OSStatus = -25300;
    const errSecInteractionNotAllowed: OSStatus = -25308;
    const errSecDecode: OSStatus = -26275;
};

pub const service_identifier = "com.phux.cockpit.provider-credential";
pub const max_secret_bytes: usize = 64 * 1024;
pub const max_registered_credentials: usize = 16;

const RegistryEntry = struct {
    active: bool = false,
    handle: store.CredentialHandle = undefined,
    reference: [store.max_persistent_reference_bytes]u8 = [_]u8{0} ** store.max_persistent_reference_bytes,
    reference_len: u16 = 0,

    fn set(entry: *RegistryEntry, handle: store.CredentialHandle, reference: []const u8) void {
        store.secureZero(&entry.reference);
        entry.handle = handle;
        @memcpy(entry.reference[0..reference.len], reference);
        entry.reference_len = @intCast(reference.len);
        entry.active = true;
    }

    fn bytes(entry: *const RegistryEntry) []const u8 {
        return entry.reference[0..entry.reference_len];
    }

    fn clear(entry: *RegistryEntry) void {
        store.secureZero(&entry.reference);
        store.secureZero(std.mem.asBytes(&entry.handle));
        entry.reference_len = 0;
        entry.active = false;
    }
};

pub const MacOSKeychain = struct {
    _entries: [max_registered_credentials]RegistryEntry = [_]RegistryEntry{.{}} ** max_registered_credentials,

    pub fn backend(keychain: *MacOSKeychain) store.Backend {
        return .{ .context = keychain, .vtable = &vtable };
    }

    pub fn deinit(keychain: *MacOSKeychain) void {
        for (&keychain._entries) |*entry| entry.clear();
    }

    pub fn format(_: *const MacOSKeychain, writer: *std.Io.Writer) std.Io.Writer.Error!void {
        try writer.writeAll("<macos-keychain>");
    }

    pub fn jsonStringify(_: *const MacOSKeychain, stringify: anytype) !void {
        try stringify.write("macos-keychain");
    }

    const vtable = store.Backend.VTable{
        .put = put,
        .open = open,
        .resolve = resolve,
        .replace = replace,
        .delete = delete,
    };

    fn self(context: *anyopaque) *MacOSKeychain {
        return @ptrCast(@alignCast(context));
    }

    fn find(keychain: *MacOSKeychain, handle: store.CredentialHandle) ?*RegistryEntry {
        for (&keychain._entries) |*entry| {
            if (entry.active and entry.handle.eql(handle)) return entry;
        }
        return null;
    }

    fn vacant(keychain: *MacOSKeychain) ?*RegistryEntry {
        for (&keychain._entries) |*entry| {
            if (!entry.active) return entry;
        }
        return null;
    }

    fn registerReopened(
        keychain: *MacOSKeychain,
        handle: store.CredentialHandle,
        reference: []const u8,
    ) store.CredentialError!void {
        if (keychain.find(handle) != null) return error.BackendFailure;
        for (&keychain._entries) |*entry| {
            if (!entry.active or !std.mem.eql(u8, entry.bytes(), reference)) continue;
            store.secureZero(std.mem.asBytes(&entry.handle));
            entry.handle = handle;
            return;
        }
        const entry = keychain.vacant() orelse return error.CapacityExceeded;
        entry.set(handle, reference);
    }

    fn put(context: *anyopaque, account: []const u8, secret: []const u8, handle: store.CredentialHandle) store.CredentialError!void {
        const keychain = self(context);
        if (account.len == 0) return error.InvalidRequest;
        if (secret.len > max_secret_bytes) return error.SecretTooLarge;
        if (keychain.find(handle) != null) return error.BackendFailure;
        const entry = keychain.vacant() orelse return error.CapacityExceeded;

        const service = try makeString(service_identifier);
        defer c.CFRelease(service);
        const account_string = try makeString(account);
        defer c.CFRelease(account_string);
        // Security.framework borrows the caller's bytes for this synchronous
        // call, avoiding an app-owned temporary secret copy that cannot be wiped.
        const secret_data = c.CFDataCreateWithBytesNoCopy(
            c.kCFAllocatorDefault,
            secret.ptr,
            @intCast(secret.len),
            c.kCFAllocatorNull,
        ) orelse return error.OutOfMemory;
        defer c.CFRelease(secret_data);

        const keys = [_]?*const anyopaque{
            c.kSecClass,
            c.kSecAttrService,
            c.kSecAttrAccount,
            c.kSecValueData,
            c.kSecReturnPersistentRef,
            c.kSecAttrAccessible,
            c.kSecUseDataProtectionKeychain,
        };
        const values = [_]?*const anyopaque{
            c.kSecClassGenericPassword,
            service,
            account_string,
            secret_data,
            c.kCFBooleanTrue,
            c.kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            c.kCFBooleanTrue,
        };
        const query = makeDictionary(&keys, &values) orelse return error.OutOfMemory;
        defer c.CFRelease(query);

        var result: c.CFTypeRef = null;
        try checkStatus(c.SecItemAdd(query, &result));
        const persistent: c.CFDataRef = @ptrCast(result orelse return error.BackendFailure);
        defer c.CFRelease(persistent);
        const length: usize = @intCast(c.CFDataGetLength(persistent));
        if (length == 0 or length > store.max_persistent_reference_bytes) {
            try deleteByReferenceData(service, persistent);
            return if (length == 0) error.InvalidHandle else error.ReferenceTooLarge;
        }
        entry.set(handle, c.CFDataGetBytePtr(persistent)[0..length]);
    }

    fn open(context: *anyopaque, account: []const u8, handle: store.CredentialHandle) store.CredentialError!void {
        const keychain = self(context);
        if (account.len == 0) return error.InvalidRequest;
        if (keychain.find(handle) != null) return error.BackendFailure;
        const service = try makeString(service_identifier);
        defer c.CFRelease(service);
        const account_string = try makeString(account);
        defer c.CFRelease(account_string);

        const keys = [_]?*const anyopaque{
            c.kSecClass,
            c.kSecAttrService,
            c.kSecAttrAccount,
            c.kSecReturnPersistentRef,
            c.kSecMatchLimit,
            c.kSecUseDataProtectionKeychain,
        };
        const values = [_]?*const anyopaque{
            c.kSecClassGenericPassword,
            service,
            account_string,
            c.kCFBooleanTrue,
            c.kSecMatchLimitOne,
            c.kCFBooleanTrue,
        };
        const query = makeDictionary(&keys, &values) orelse return error.OutOfMemory;
        defer c.CFRelease(query);

        var result: c.CFTypeRef = null;
        try checkStatus(c.SecItemCopyMatching(query, &result));
        const persistent: c.CFDataRef = @ptrCast(result orelse return error.BackendFailure);
        defer c.CFRelease(persistent);
        const length: usize = @intCast(c.CFDataGetLength(persistent));
        if (length == 0) return error.InvalidHandle;
        if (length > store.max_persistent_reference_bytes) return error.ReferenceTooLarge;
        try keychain.registerReopened(handle, c.CFDataGetBytePtr(persistent)[0..length]);
    }

    fn resolve(context: *anyopaque, handle: store.CredentialHandle, destination: []u8) store.CredentialError!usize {
        const entry = self(context).find(handle) orelse return error.InvalidHandle;
        const service = try makeString(service_identifier);
        defer c.CFRelease(service);
        const reference_data = try makeData(entry.bytes());
        defer c.CFRelease(reference_data);

        const keys = [_]?*const anyopaque{
            c.kSecClass,
            c.kSecAttrService,
            c.kSecValuePersistentRef,
            c.kSecReturnData,
            c.kSecMatchLimit,
            c.kSecUseDataProtectionKeychain,
        };
        const values = [_]?*const anyopaque{
            c.kSecClassGenericPassword,
            service,
            reference_data,
            c.kCFBooleanTrue,
            c.kSecMatchLimitOne,
            c.kCFBooleanTrue,
        };
        const query = makeDictionary(&keys, &values) orelse return error.OutOfMemory;
        defer c.CFRelease(query);

        var result: c.CFTypeRef = null;
        try checkStatus(c.SecItemCopyMatching(query, &result));
        const secret_data: c.CFDataRef = @ptrCast(result orelse return error.BackendFailure);
        defer c.CFRelease(secret_data);
        const length: usize = @intCast(c.CFDataGetLength(secret_data));
        if (length > destination.len) return error.BufferTooSmall;
        if (length != 0) @memcpy(destination[0..length], c.CFDataGetBytePtr(secret_data)[0..length]);
        return length;
    }

    fn replace(context: *anyopaque, handle: store.CredentialHandle, secret: []const u8) store.CredentialError!void {
        if (secret.len > max_secret_bytes) return error.SecretTooLarge;
        const entry = self(context).find(handle) orelse return error.InvalidHandle;
        const service = try makeString(service_identifier);
        defer c.CFRelease(service);
        const reference_data = try makeData(entry.bytes());
        defer c.CFRelease(reference_data);
        const secret_data = c.CFDataCreateWithBytesNoCopy(
            c.kCFAllocatorDefault,
            secret.ptr,
            @intCast(secret.len),
            c.kCFAllocatorNull,
        ) orelse return error.OutOfMemory;
        defer c.CFRelease(secret_data);

        const query_keys = [_]?*const anyopaque{
            c.kSecClass,
            c.kSecAttrService,
            c.kSecValuePersistentRef,
            c.kSecUseDataProtectionKeychain,
        };
        const query_values = [_]?*const anyopaque{
            c.kSecClassGenericPassword,
            service,
            reference_data,
            c.kCFBooleanTrue,
        };
        const query = makeDictionary(&query_keys, &query_values) orelse return error.OutOfMemory;
        defer c.CFRelease(query);
        const update_keys = [_]?*const anyopaque{c.kSecValueData};
        const update_values = [_]?*const anyopaque{secret_data};
        const update = makeDictionary(&update_keys, &update_values) orelse return error.OutOfMemory;
        defer c.CFRelease(update);
        try checkStatus(c.SecItemUpdate(query, update));
    }

    fn delete(context: *anyopaque, handle: store.CredentialHandle) store.CredentialError!void {
        const entry = self(context).find(handle) orelse return error.InvalidHandle;
        const service = try makeString(service_identifier);
        defer c.CFRelease(service);
        const reference_data = try makeData(entry.bytes());
        defer c.CFRelease(reference_data);
        try deleteByReferenceData(service, reference_data);
        entry.clear();
    }
};

fn makeString(bytes: []const u8) store.CredentialError!c.CFStringRef {
    return c.CFStringCreateWithBytes(
        c.kCFAllocatorDefault,
        bytes.ptr,
        @intCast(bytes.len),
        c.kCFStringEncodingUTF8,
        0,
    ) orelse error.OutOfMemory;
}

fn makeData(bytes: []const u8) store.CredentialError!c.CFDataRef {
    return c.CFDataCreate(c.kCFAllocatorDefault, bytes.ptr, @intCast(bytes.len)) orelse error.OutOfMemory;
}

fn makeDictionary(keys: anytype, values: anytype) c.CFDictionaryRef {
    return c.CFDictionaryCreate(
        c.kCFAllocatorDefault,
        @ptrCast(keys),
        @ptrCast(values),
        @intCast(keys.len),
        &c.kCFTypeDictionaryKeyCallBacks,
        &c.kCFTypeDictionaryValueCallBacks,
    );
}

fn deleteByReferenceData(service: c.CFStringRef, reference_data: c.CFDataRef) store.CredentialError!void {
    const items = [_]?*const anyopaque{reference_data};
    const item_list = c.CFArrayCreate(
        c.kCFAllocatorDefault,
        @ptrCast(&items),
        items.len,
        &c.kCFTypeArrayCallBacks,
    ) orelse return error.OutOfMemory;
    defer c.CFRelease(item_list);
    const keys = [_]?*const anyopaque{
        c.kSecClass,
        c.kSecAttrService,
        c.kSecMatchItemList,
        c.kSecUseDataProtectionKeychain,
    };
    const values = [_]?*const anyopaque{
        c.kSecClassGenericPassword,
        service,
        item_list,
        c.kCFBooleanTrue,
    };
    const query = makeDictionary(&keys, &values) orelse return error.OutOfMemory;
    defer c.CFRelease(query);
    try checkStatus(c.SecItemDelete(query));
}

fn checkStatus(status: c.OSStatus) store.CredentialError!void {
    if (status == c.errSecSuccess) return;
    return switch (status) {
        c.errSecDuplicateItem => error.AlreadyExists,
        c.errSecItemNotFound => error.NotFound,
        c.errSecAuthFailed => error.AccessDenied,
        c.errSecInteractionNotAllowed => error.InteractionRequired,
        c.errSecDecode => error.CorruptItem,
        c.errSecParam => error.InvalidRequest,
        c.errSecAllocate => error.OutOfMemory,
        else => error.BackendFailure,
    };
}

test "macOS Keychain backend declarations compile" {
    var keychain = MacOSKeychain{};
    defer keychain.deinit();
    _ = keychain.backend();
}

const TokenIssuer = struct {
    fn backend(issuer: *TokenIssuer) store.Backend {
        return .{ .context = issuer, .vtable = &vtable };
    }

    const vtable = store.Backend.VTable{
        .put = put,
        .open = open,
        .resolve = resolve,
        .replace = replace,
        .delete = delete,
    };

    fn put(_: *anyopaque, _: []const u8, _: []const u8, _: store.CredentialHandle) store.CredentialError!void {}
    fn open(_: *anyopaque, _: []const u8, _: store.CredentialHandle) store.CredentialError!void {}
    fn resolve(_: *anyopaque, _: store.CredentialHandle, _: []u8) store.CredentialError!usize {
        return error.BackendFailure;
    }
    fn replace(_: *anyopaque, _: store.CredentialHandle, _: []const u8) store.CredentialError!void {
        return error.BackendFailure;
    }
    fn delete(_: *anyopaque, _: store.CredentialHandle) store.CredentialError!void {
        return error.BackendFailure;
    }
};

test "process registry rejects foreign and deleted handles and clears references" {
    var issuer = TokenIssuer{};
    const token_backend = issuer.backend();
    var keychain = MacOSKeychain{};
    var handles: [max_registered_credentials]store.CredentialHandle = undefined;
    for (&keychain._entries, 0..) |*entry, index| {
        handles[index] = try token_backend.put(std.testing.io, "issuer", "not-stored");
        entry.set(handles[index], "persistent-reference");
    }
    const foreign = try token_backend.put(std.testing.io, "issuer", "not-stored");
    try std.testing.expect(keychain.find(foreign) == null);
    try std.testing.expect(keychain.vacant() == null);
    try keychain.registerReopened(foreign, "persistent-reference");
    try std.testing.expect(keychain.find(foreign) != null);
    try std.testing.expect(keychain.find(handles[0]) == null);
    try std.testing.expect(keychain.vacant() == null);

    keychain._entries[0].clear();
    try std.testing.expect(keychain.find(handles[0]) == null);
    try std.testing.expect(allZero(&keychain._entries[0].reference));

    keychain._entries[0].set(foreign, "replacement-reference");
    keychain.deinit();
    for (&keychain._entries) |*entry| {
        try std.testing.expect(!entry.active);
        try std.testing.expectEqual(@as(u16, 0), entry.reference_len);
        try std.testing.expect(allZero(&entry.reference));
    }
}

fn allZero(bytes: []const u8) bool {
    for (bytes) |byte| if (byte != 0) return false;
    return true;
}
