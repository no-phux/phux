//! Owning Zig face of the caller-bounded shared CLI registry snapshot.
const std = @import("std");
const remote = @import("phux_extension").remote;
const c = remote.registryAbi();
pub const Tunnel = remote.Tunnel;
pub const Error = error{ RegistryUnavailable, StaleRegistry, InvalidRow };

pub const Record = struct {
    role: u32,
    route: u32,
    name: []const u8,
    endpoint: []const u8,
    session: []const u8,
    message: []const u8,
};

fn text(span: c.PhuxBytes) []const u8 {
    return if (span.len == 0) "" else span.data[0..span.len];
}

pub const Registry = struct {
    handle: *c.PhuxMachineRegistry,
    count: usize,
    failed: bool,
    message: []const u8,

    pub fn open(path: []const u8, max_entries: usize, max_file_bytes: usize) Error!Registry {
        var options = std.mem.zeroes(c.PhuxMachineRegistryOptions);
        options.size = @sizeOf(c.PhuxMachineRegistryOptions);
        options.version = c.PHUX_CLIENT_ABI_VERSION;
        options.config_path = .{ .data = path.ptr, .len = path.len };
        options.max_entries = max_entries;
        options.max_file_bytes = max_file_bytes;
        var handle: ?*c.PhuxMachineRegistry = null;
        if (c.phux_machine_registry_open(&options, &handle) != c.PHUX_CLIENT_OK) return error.RegistryUnavailable;
        const owned = handle orelse return error.RegistryUnavailable;
        errdefer c.phux_machine_registry_free(owned);
        var info = std.mem.zeroes(c.PhuxMachineRegistryInfo);
        info.size = @sizeOf(c.PhuxMachineRegistryInfo);
        info.version = c.PHUX_CLIENT_ABI_VERSION;
        if (c.phux_machine_registry_info(owned, &info) != c.PHUX_CLIENT_OK) return error.RegistryUnavailable;
        return .{ .handle = owned, .count = info.count, .failed = info.failed != 0, .message = text(info.message) };
    }

    pub fn close(self: Registry) void {
        c.phux_machine_registry_free(self.handle);
    }

    pub fn get(self: Registry, index: usize) Error!Record {
        var row = std.mem.zeroes(c.PhuxMachineRecord);
        row.size = @sizeOf(c.PhuxMachineRecord);
        row.version = c.PHUX_CLIENT_ABI_VERSION;
        if (c.phux_machine_registry_get(self.handle, index, &row) != c.PHUX_CLIENT_OK) return error.InvalidRow;
        return .{ .role = row.role, .route = row.route, .name = text(row.name), .endpoint = text(row.endpoint), .session = text(row.session), .message = text(row.message) };
    }

    pub fn validate(self: Registry, index: usize) Error!void {
        if (c.phux_machine_registry_validate(self.handle, index) != c.PHUX_CLIENT_OK) return error.StaleRegistry;
    }

    pub fn resolve(self: Registry, index: usize) Error!Tunnel {
        var tunnel: ?*c.PhuxRemoteTunnel = null;
        if (c.phux_machine_registry_resolve(self.handle, index, &tunnel) != c.PHUX_CLIENT_OK) return error.StaleRegistry;
        return .{ .handle = tunnel orelse return error.RegistryUnavailable };
    }

    pub fn forget(self: Registry, index: usize) Error!void {
        if (c.phux_machine_registry_forget(self.handle, index) != c.PHUX_CLIENT_OK) return error.RegistryUnavailable;
    }
};
