//! Dedicated machine inventory request. Terminal snapshots never perform registry I/O.
//! Integers are little-endian; strings are u16 byte length followed by UTF-8.
//! Request (16 bytes): v1:u8,op:u8,id:u32,generation:u32,row/first:u32,limit:u16.
//! Reply: v1:u8,status:u8,id:u32,generation:u32,total:u32,first:u32,count:u16,
//! message:string16, then count rows: index:u32,role:u8,route:u8,state:u8,
//! name:string16,endpoint:string16,session:string16,message:string16.
const std = @import("std");
const phux_options = @import("phux_options");
const api = @import("phux_provider").machines;
const Registry = if (phux_options.enabled) api.Registry else DisabledRegistry;
pub const Tunnel = if (phux_options.enabled) api.Tunnel else struct {};
pub const request_name = "cockpit.machines";
pub const max_bytes = 65536;
// Four text fields per row plus one receipt diagnostic remain comfortably below
// max_bytes even after a command changes all diagnostics. Identity is never
// decoded from these display strings; activation uses the captured registry row.
pub const max_text_bytes = 1024;
pub const Error = error{ InvalidRequest, BufferTooSmall, GenerationExhausted };
pub const Role = enum(u8) { local = 0, remote = 1, satellite = 2 };
pub const Route = enum(u8) { local = 0, direct = 1, needs_setup = 2, via_hub = 3, disabled = 4, unsupported = 5 };
pub const Connection = enum(u8) { not_connected = 0, connecting = 1, connected = 2, reconnecting = 3, failed = 4 };
pub const Action = enum(u8) { connect = 2, retry = 3, disconnect = 4, browse = 7 };
pub const ReplyStatus = enum(u8) { ok = 0, failed = 1, stale = 2, unsupported = 3 };
pub const Identity = struct { role: Role, name: []const u8, endpoint: []const u8, session: []const u8 };
/// Message storage is caller/provider-owned and remains valid until the next
/// status callback. Never return a slice into the callback's stack frame.
pub const Status = struct { state: Connection = .not_connected, message: []const u8 = "" };
/// Message storage remains valid through handle's reply encoding. Keep it
/// separate from mutable status scratch storage used while encoding row states.
pub const ActionResult = struct { status: ReplyStatus = .ok, message: []const u8 = "" };

/// Callbacks must join role/name/endpoint, never alias alone. Identity strings are
/// borrowed for this call; copy them when queueing. Action owns Tunnel on EVERY
/// return path and transfers that checked tunnel to its worker (no alias lookup).
pub const Context = struct {
    userdata: ?*anyopaque = null,
    status: ?*const fn (?*anyopaque, Identity) Status = null,
    action: ?*const fn (?*anyopaque, Action, Identity, ?Tunnel) ActionResult = null,

    fn current(self: Context, identity: Identity) Status {
        const callback = self.status orelse return .{};
        return callback(self.userdata, identity);
    }
};

pub const State = struct {
    config_path: []const u8 = "",
    // Caller-overridable resource budgets, independent of provider capacity.
    max_entries: usize = 10000,
    max_file_bytes: usize = 4 * 1024 * 1024,
    generation: u32 = 0,
    registry: ?Registry = null,

    pub fn deinit(self: *State) void {
        if (self.registry) |registry| registry.close();
        self.registry = null;
    }

    fn invalidate(self: *State) Error!void {
        self.deinit();
        self.generation = std.math.add(u32, self.generation, 1) catch return error.GenerationExhausted;
    }

    fn refresh(self: *State) Error!void {
        try self.invalidate();
        self.registry = Registry.open(self.config_path, self.max_entries, self.max_file_bytes) catch null;
    }

    fn total(self: *const State) u32 {
        const registry = self.registry orelse return 1;
        return @intCast(@min(registry.count, std.math.maxInt(u32) - 1) + 1);
    }

    fn row(self: *const State, index: u32) ?Row {
        if (index == 0) return .{ .identity = .{ .role = .local, .name = "This Mac", .endpoint = "", .session = "" }, .route = .local };
        const registry = self.registry orelse return null;
        const record = registry.get(index - 1) catch return null;
        return .{ .identity = .{ .role = std.enums.fromInt(Role, record.role) orelse return null, .name = record.name, .endpoint = record.endpoint, .session = record.session }, .route = std.enums.fromInt(Route, record.route) orelse .unsupported, .message = record.message };
    }
};

const Row = struct { identity: Identity, route: Route, message: []const u8 = "" };
const Request = struct {
    op: u8,
    id: u32,
    generation: u32,
    first: u32,
    limit: u16,
    fn decode(bytes: []const u8) Error!Request {
        if (bytes.len != 16 or bytes[0] != 1 or bytes[1] > 7) return error.InvalidRequest;
        return .{ .op = bytes[1], .id = std.mem.readInt(u32, bytes[2..6], .little), .generation = std.mem.readInt(u32, bytes[6..10], .little), .first = std.mem.readInt(u32, bytes[10..14], .little), .limit = std.mem.readInt(u16, bytes[14..16], .little) };
    }
};

/// Synchronous request/response; refresh/cancel retire prior captured actions.
/// Caller correlates request id before applying a reply and cancels focus intent
/// separately in the engine. A late connection never grants focus here.
pub fn handle(state: *State, context: Context, input: []const u8, output: []u8) Error![]const u8 {
    const request = try Request.decode(input);
    if (output.len < 22) return error.BufferTooSmall;
    if (request.op == 6) {
        try state.invalidate();
        return reply(state, context, request, .{}, 0, 0, output);
    }
    if (request.op == 0) {
        try state.refresh();
        return page(state, context, request, output);
    }
    if (state.generation == 0 or request.generation != state.generation) {
        return reply(state, context, request, .{ .status = .stale, .message = "Machines changed; refresh and select again" }, 0, 0, output);
    }
    if (request.op == 1) return page(state, context, request, output);
    // Prove the captured row can be returned before a side effect. Command
    // callers reserve the full reply budget, including a failure receipt.
    if (output.len < max_bytes) return error.BufferTooSmall;
    _ = try reply(state, context, request, .{}, request.first, 1, output);
    if (request.op == 5 and state.generation == std.math.maxInt(u32)) {
        return reply(state, context, request, .{ .status = .failed, .message = "Machines capture capacity exhausted; reopen Cockpit before forgetting" }, 0, 0, output);
    }
    const result = activate(state, context, request);
    return commandReply(state, context, request, result, output);
}

// After activation, even an unexpected encoding/refresh failure must produce a
// correlated receipt. Caller capacity was proven before invoking the mutation.
fn commandReply(state: *State, context: Context, request: Request, result: ActionResult, output: []u8) []const u8 {
    if (result.status != .ok) return reply(state, context, request, result, 0, 0, output) catch fallbackReceipt(state, request, result.status, output);
    if (request.op == 5) {
        state.refresh() catch return fallbackReceipt(state, request, result.status, output);
        const registry = state.registry orelse return fallbackReceipt(state, request, result.status, output);
        if (registry.failed) return fallbackReceipt(state, request, result.status, output);
        var refreshed = request;
        refreshed.first = 0;
        return page(state, context, refreshed, output) catch fallbackReceipt(state, request, result.status, output);
    }
    return reply(state, context, request, result, request.first, 1, output) catch fallbackReceipt(state, request, result.status, output);
}

fn fallbackReceipt(state: *State, request: Request, status: ReplyStatus, output: []u8) []const u8 {
    const message = "Machine action settled; refresh Machines for current details";
    output[0] = 1;
    output[1] = @intFromEnum(status);
    std.mem.writeInt(u32, output[2..6], request.id, .little);
    std.mem.writeInt(u32, output[6..10], state.generation, .little);
    std.mem.writeInt(u32, output[10..14], state.total(), .little);
    @memset(output[14..20], 0);
    std.mem.writeInt(u16, output[20..22], message.len, .little);
    @memcpy(output[22..][0..message.len], message);
    return output[0 .. 22 + message.len];
}

fn page(state: *State, context: Context, request: Request, output: []u8) Error![]const u8 {
    const registry = state.registry orelse return reply(state, context, request, .{ .status = .failed, .message = "Machine registry unavailable; check Phux configuration" }, 0, 1, output);
    if (registry.failed) return reply(state, context, request, .{ .status = .failed, .message = registry.message }, 0, 1, output);
    return reply(state, context, request, .{}, request.first, request.limit, output);
}

fn capturedRow(state: *State, index: u32) error{StaleRegistry}!Row {
    const row = state.row(index) orelse return error.StaleRegistry;
    if (index > 0) {
        const registry = state.registry orelse return error.StaleRegistry;
        registry.validate(index - 1) catch return error.StaleRegistry;
    }
    return row;
}

fn activate(state: *State, context: Context, request: Request) ActionResult {
    const row = capturedRow(state, request.first) catch return .{ .status = .stale, .message = "Machine registry changed or is unreadable; refresh Machines" };
    if (row.route == .unsupported) return .{ .status = .unsupported, .message = row.message };
    const current = context.current(row.identity);
    if (request.op == 5) return forget(state, request.first, current);
    const action = std.enums.fromInt(Action, request.op) orelse return .{ .status = .unsupported };
    if (action == .connect or action == .retry) return connect(state, context, request.first, row, current, action);
    if (action == .disconnect and row.identity.role != .remote) return .{ .status = .unsupported, .message = "Only direct remote connections can be disconnected" };
    const callback = context.action orelse return .{ .status = .unsupported, .message = "Machine action is unavailable in this runtime" };
    return callback(context.userdata, action, row.identity, null);
}

fn forget(state: *State, index: u32, current: Status) ActionResult {
    if (index == 0) return .{ .status = .unsupported, .message = "This Mac is not a saved remote registration" };
    switch (current.state) {
        .connected, .connecting, .reconnecting => return .{ .status = .failed, .message = "Disconnect this machine before forgetting it" },
        else => {},
    }
    const registry = state.registry orelse return .{ .status = .stale };
    registry.forget(index - 1) catch return .{ .status = .failed, .message = "Could not forget this exact entry; registry may be busy, unwritable, inherited or changed. Refresh and retry" };
    return .{};
}

fn connect(state: *State, context: Context, index: u32, row: Row, current: Status, action: Action) ActionResult {
    if (row.route != .direct) return .{ .status = .unsupported, .message = row.message };
    switch (current.state) {
        .connected, .connecting, .reconnecting => return .{}, // double activation joins the same attempt
        else => {},
    }
    const callback = context.action orelse return .{ .status = .unsupported, .message = "Machine connection is unavailable in this runtime" };
    const registry = state.registry orelse return .{ .status = .stale };
    const tunnel = registry.resolve(index - 1) catch return .{ .status = .stale, .message = "Saved destination changed; refresh Machines" };
    return callback(context.userdata, action, row.identity, tunnel);
}

const Writer = struct {
    bytes: []u8,
    used: usize = 0,
    fn int(self: *Writer, comptime T: type, value: T) Error!void {
        const len = @sizeOf(T);
        if (self.bytes.len - self.used < len) return error.BufferTooSmall;
        std.mem.writeInt(T, self.bytes[self.used..][0..len], value, .little);
        self.used += len;
    }
    fn text(self: *Writer, value: []const u8) Error!void {
        const clipped = value.len > max_text_bytes;
        var end = @min(value.len, max_text_bytes);
        if (clipped) {
            end -= "…".len;
            while (end > 0 and (value[end] & 0xc0) == 0x80) end -= 1;
        }
        const prefix = if (std.unicode.utf8ValidateSlice(value[0..end])) value[0..end] else "Invalid UTF-8 diagnostic";
        const suffix: []const u8 = if (clipped) "…" else "";
        const length = prefix.len + suffix.len;
        if (self.bytes.len - self.used < 2 + length) return error.BufferTooSmall;
        try self.int(u16, @intCast(length));
        @memcpy(self.bytes[self.used..][0..prefix.len], prefix);
        self.used += prefix.len;
        @memcpy(self.bytes[self.used..][0..suffix.len], suffix);
        self.used += suffix.len;
    }
};

fn writeRow(writer: *Writer, index: u32, row: Row, status: Status) Error!void {
    try writer.int(u32, index);
    try writer.int(u8, @intFromEnum(row.identity.role));
    try writer.int(u8, @intFromEnum(row.route));
    try writer.int(u8, @intFromEnum(status.state));
    try writer.text(row.identity.name);
    try writer.text(row.identity.endpoint);
    try writer.text(row.identity.session);
    try writer.text(if (status.message.len > 0) status.message else row.message);
}

fn reply(state: *State, context: Context, request: Request, result: ActionResult, first: u32, limit: u16, output: []u8) Error![]const u8 {
    var writer: Writer = .{ .bytes = output[0..@min(output.len, max_bytes)] };
    try writer.int(u8, 1);
    try writer.int(u8, @intFromEnum(result.status));
    try writer.int(u32, request.id);
    try writer.int(u32, state.generation);
    try writer.int(u32, state.total());
    try writer.int(u32, first);
    try writer.int(u16, 0);
    try writer.text(result.message);
    var count: u16 = 0;
    while (count < limit) : (count += 1) {
        const index = std.math.add(u32, first, count) catch break;
        const row = state.row(index) orelse break;
        const before = writer.used;
        const status = if (row.route == .unsupported) Status{ .state = .failed, .message = row.message } else context.current(row.identity);
        writeRow(&writer, index, row, status) catch {
            writer.used = before;
            if (count == 0) return error.BufferTooSmall;
            break;
        };
    }
    std.mem.writeInt(u16, writer.bytes[18..20], count, .little);
    return writer.bytes[0..writer.used];
}

const DisabledRegistry = struct {
    count: usize = 0,
    failed: bool = false,
    message: []const u8 = "",
    fn open(_: []const u8, _: usize, _: usize) error{}!DisabledRegistry {
        return .{};
    }
    fn close(_: DisabledRegistry) void {}
    fn get(_: DisabledRegistry, _: usize) error{InvalidRow}!api.Record {
        return error.InvalidRow;
    }
    fn validate(_: DisabledRegistry, _: usize) error{StaleRegistry}!void {
        return error.StaleRegistry;
    }
    fn forget(_: DisabledRegistry, _: usize) error{RegistryUnavailable}!void {
        return error.RegistryUnavailable;
    }
    fn resolve(_: DisabledRegistry, _: usize) error{StaleRegistry}!Tunnel {
        return error.StaleRegistry;
    }
};

test "machine request is independently bounded and cancel invalidates capture" {
    var state: State = .{};
    defer state.deinit();
    var input = [_]u8{0} ** 16;
    input[0] = 1;
    input[1] = 6;
    std.mem.writeInt(u32, input[2..6], 37, .little);
    var output: [1024]u8 = undefined;
    const canceled = try handle(&state, .{}, &input, &output);
    try std.testing.expectEqual(@as(u32, 37), std.mem.readInt(u32, canceled[2..6], .little));
    try std.testing.expectEqual(@as(u32, 1), state.generation);
    input[1] = 2;
    const stale = try handle(&state, .{}, &input, &output);
    try std.testing.expectEqual(@as(u8, 2), stale[1]);
    try std.testing.expectError(error.InvalidRequest, handle(&state, .{}, input[0..15], &output));
    try std.testing.expectError(error.BufferTooSmall, handle(&state, .{}, &input, output[0..4]));
}

test "status joins the captured full identity and does not claim saved reachability" {
    const Probe = struct {
        fn status(_: ?*anyopaque, identity: Identity) Status {
            if (std.mem.eql(u8, identity.name, "same") and std.mem.eql(u8, identity.endpoint, "ws://right:1")) return .{ .state = .connected };
            return .{};
        }
    };
    const context: Context = .{ .status = Probe.status };
    try std.testing.expectEqual(Connection.connected, context.current(.{ .role = .remote, .name = "same", .endpoint = "ws://right:1", .session = "" }).state);
    try std.testing.expectEqual(Connection.not_connected, context.current(.{ .role = .remote, .name = "same", .endpoint = "ws://wrong:1", .session = "" }).state);
    try std.testing.expectEqual(Connection.not_connected, (Context{}).current(.{ .role = .local, .name = "This Mac", .endpoint = "", .session = "" }).state);
}

test "hermetic Machines list, exact connect, double activation, disconnect, forget and stale edit" {
    if (!phux_options.enabled) return error.SkipZigTest;
    const remote = @import("phux_provider").remote_api;
    var fixture = try remote.TestRegistry.init("machine", "ws://localhost:1");
    defer fixture.deinit();
    var state: State = .{ .config_path = fixture.path };
    defer state.deinit();
    const Probe = struct {
        attempts: usize = 0,
        connection: Connection = .not_connected,
        fn status(raw: ?*anyopaque, identity: Identity) Status {
            const self: *@This() = @ptrCast(@alignCast(raw.?));
            if (identity.role != .remote or !std.mem.eql(u8, identity.endpoint, "ws://localhost:1")) return .{};
            return .{ .state = self.connection };
        }
        fn action(raw: ?*anyopaque, kind: Action, identity: Identity, tunnel: ?Tunnel) ActionResult {
            const self: *@This() = @ptrCast(@alignCast(raw.?));
            if (tunnel) |owned| {
                defer owned.close();
                const description = owned.describe();
                std.debug.assert(std.mem.eql(u8, description.endpoint.slice(), identity.endpoint));
                self.attempts += 1;
                self.connection = .connecting;
            }
            if (kind == .disconnect) self.connection = .not_connected;
            return .{};
        }
    };
    var probe: Probe = .{};
    const context: Context = .{ .userdata = &probe, .status = Probe.status, .action = Probe.action };
    var input = [_]u8{0} ** 16;
    input[0] = 1;
    std.mem.writeInt(u16, input[14..16], 64, .little);
    var output: [max_bytes]u8 = undefined;
    const listed = try handle(&state, context, &input, &output);
    try std.testing.expectEqual(@as(u8, 0), listed[1]);
    try std.testing.expectEqual(@as(u16, 2), std.mem.readInt(u16, listed[18..20], .little));
    std.mem.writeInt(u32, input[6..10], state.generation, .little);
    std.mem.writeInt(u32, input[10..14], 1, .little);
    input[1] = 2;
    try std.testing.expectError(error.BufferTooSmall, handle(&state, context, &input, output[0..32]));
    try std.testing.expectEqual(@as(usize, 0), probe.attempts);
    _ = try handle(&state, context, &input, &output);
    _ = try handle(&state, context, &input, &output);
    try std.testing.expectEqual(@as(usize, 1), probe.attempts);
    input[1] = 5;
    const refused = try handle(&state, context, &input, &output);
    try std.testing.expectEqual(@as(u8, 1), refused[1]);
    input[1] = 4;
    _ = try handle(&state, context, &input, &output);
    input[1] = 5;
    const forgotten = try handle(&state, context, &input, &output);
    try std.testing.expectEqual(@as(u8, 0), forgotten[1]);
    try std.testing.expectEqual(@as(u32, 1), std.mem.readInt(u32, forgotten[10..14], .little));
    const stale = try handle(&state, context, &input, &output);
    try std.testing.expectEqual(@as(u8, 2), stale[1]);
    try fixture.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config.toml", .data = "[[remote]]\nname='machine'\nendpoint='ws://localhost:1'\n" });
    input[1] = 0;
    std.mem.writeInt(u32, input[10..14], 0, .little);
    _ = try handle(&state, context, &input, &output);
    std.mem.writeInt(u32, input[6..10], state.generation, .little);
    std.mem.writeInt(u32, input[10..14], 1, .little);
    try fixture.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config.toml", .data = "[[remote]]\nname='machine'\nendpoint='ws://localhost:2'\n" });
    input[1] = 3;
    const edited = try handle(&state, context, &input, &output);
    try std.testing.expectEqual(@as(u8, 2), edited[1]);
    try std.testing.expectEqual(@as(usize, 1), probe.attempts);
}

test "registry pagination returns more than four entries and retains local row on malformed input" {
    if (!phux_options.enabled) return error.SkipZigTest;
    const remote = @import("phux_provider").remote_api;
    var fixture = try remote.TestRegistry.init("machine", "ws://localhost:1");
    defer fixture.deinit();
    const body = "[[remote]]\nname='one'\nendpoint='ssh://one'\n" ++
        "[[remote]]\nname='two'\nendpoint='ssh://two'\n" ++
        "[[remote]]\nname='three'\nendpoint='ssh://three'\n" ++
        "[[remote]]\nname='four'\nendpoint='ssh://four'\n" ++
        "[[remote]]\nname='five'\nendpoint='ssh://five'\n" ++
        "[[satellites]]\nname='six'\nendpoint='ssh://six'\n";
    try fixture.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config.toml", .data = body });
    var state: State = .{ .config_path = fixture.path };
    defer state.deinit();
    var input = [_]u8{0} ** 16;
    input[0] = 1;
    std.mem.writeInt(u16, input[14..16], 64, .little);
    var output: [max_bytes]u8 = undefined;
    const all = try handle(&state, .{}, &input, &output);
    try std.testing.expectEqual(@as(u16, 7), std.mem.readInt(u16, all[18..20], .little));
    input[1] = 1;
    std.mem.writeInt(u32, input[6..10], state.generation, .little);
    std.mem.writeInt(u32, input[10..14], 5, .little);
    const tail = try handle(&state, .{}, &input, &output);
    try std.testing.expectEqual(@as(u16, 2), std.mem.readInt(u16, tail[18..20], .little));
    try fixture.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config.toml", .data = "[[broken" });
    input[1] = 0;
    const malformed = try handle(&state, .{}, &input, &output);
    try std.testing.expectEqual(@as(u8, 1), malformed[1]);
    try std.testing.expectEqual(@as(u16, 1), std.mem.readInt(u16, malformed[18..20], .little));
}

test "review post-action diagnostics cannot lose an already applied command receipt" {
    if (!phux_options.enabled) return error.SkipZigTest;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config.toml", .data = "[[remote]]\nname='machine'\nendpoint='ws://localhost:1'\n" });
    const path = try tmp.dir.realPathFileAlloc(std.testing.io, "config.toml", std.testing.allocator);
    defer std.testing.allocator.free(path);
    const diagnostic = try std.testing.allocator.alloc(u8, 90000);
    defer std.testing.allocator.free(diagnostic);
    @memset(diagnostic, 'x');
    const Probe = struct {
        diagnostic: []const u8,
        mutated: bool = false,
        action_diagnostic: bool = true,
        fn status(raw: ?*anyopaque, _: Identity) Status {
            const self: *@This() = @ptrCast(@alignCast(raw.?));
            return .{ .state = .failed, .message = if (self.mutated) self.diagnostic else "" };
        }
        fn action(raw: ?*anyopaque, _: Action, _: Identity, tunnel: ?Tunnel) ActionResult {
            const self: *@This() = @ptrCast(@alignCast(raw.?));
            if (tunnel) |owned| owned.close();
            self.mutated = true;
            return .{ .message = if (self.action_diagnostic) self.diagnostic else "" };
        }
    };
    var probe: Probe = .{ .diagnostic = diagnostic };
    const context: Context = .{ .userdata = &probe, .status = Probe.status, .action = Probe.action };
    var state: State = .{ .config_path = path };
    defer state.deinit();
    var input = [_]u8{0} ** 16;
    input[0] = 1;
    std.mem.writeInt(u16, input[14..16], 1, .little);
    var output: [max_bytes]u8 = undefined;
    _ = try handle(&state, context, &input, &output);
    input[1] = 2;
    std.mem.writeInt(u32, input[6..10], state.generation, .little);
    std.mem.writeInt(u32, input[10..14], 1, .little);
    const outcome = handle(&state, context, &input, &output);
    try std.testing.expect(probe.mutated);
    const receipt = try outcome;
    try std.testing.expectEqual(@as(u8, 0), receipt[1]);
    try std.testing.expect(receipt.len <= max_bytes);
    probe.mutated = false;
    probe.action_diagnostic = false;
    input[1] = 3;
    const status_only_outcome = handle(&state, context, &input, &output);
    try std.testing.expect(probe.mutated);
    const status_only = try status_only_outcome;
    try std.testing.expectEqual(@as(u8, 0), status_only[1]);
    try std.testing.expectEqual(@as(u16, 1), std.mem.readInt(u16, status_only[18..20], .little));
}

test "display diagnostics elide on UTF-8 boundaries and fallback preserves correlation" {
    var text: [2000 * 3]u8 = undefined;
    for (0..2000) |i| @memcpy(text[i * 3 ..][0..3], "界");
    var output: [max_bytes]u8 = undefined;
    var writer: Writer = .{ .bytes = &output };
    try writer.text(&text);
    const len = std.mem.readInt(u16, output[0..2], .little);
    try std.testing.expect(len <= max_text_bytes);
    try std.testing.expect(std.unicode.utf8ValidateSlice(output[2..writer.used]));
    try std.testing.expect(std.mem.endsWith(u8, output[2..writer.used], "…"));
    var state: State = .{ .generation = 17 };
    const request: Request = .{ .op = 2, .id = 42, .generation = 17, .first = 1, .limit = 1 };
    const fallback = fallbackReceipt(&state, request, .failed, &output);
    try std.testing.expectEqual(@as(u8, 1), fallback[1]);
    try std.testing.expectEqual(@as(u32, 42), std.mem.readInt(u32, fallback[2..6], .little));
    try std.testing.expectEqual(@as(u32, 17), std.mem.readInt(u32, fallback[6..10], .little));
    try std.testing.expectEqual(@as(u16, 0), std.mem.readInt(u16, fallback[18..20], .little));
    if (phux_options.enabled) {
        // Invalid capture budget makes refresh fail after a settled Forget.
        // The receipt must retain the action's success, not claim it failed.
        state.max_file_bytes = 0;
        var forgotten_request = request;
        forgotten_request.op = 5;
        const settled = commandReply(&state, .{}, forgotten_request, .{}, &output);
        try std.testing.expectEqual(@as(u8, 0), settled[1]);
        try std.testing.expectEqual(@as(u32, 42), std.mem.readInt(u32, settled[2..6], .little));
        try std.testing.expectEqual(@as(u16, 0), std.mem.readInt(u16, settled[18..20], .little));
    }
}

test "review oversized first saved field remains pageable without changing capture authority" {
    if (!phux_options.enabled) return error.SkipZigTest;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const name = try std.testing.allocator.alloc(u8, 70000);
    defer std.testing.allocator.free(name);
    @memset(name, 'a');
    const body = try std.fmt.allocPrint(std.testing.allocator, "[[remote]]\nname='{s}'\nendpoint='ws://localhost:1'\n[[remote]]\nname='next'\nendpoint='ws://localhost:2'\n", .{name});
    defer std.testing.allocator.free(body);
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config.toml", .data = body });
    const path = try tmp.dir.realPathFileAlloc(std.testing.io, "config.toml", std.testing.allocator);
    defer std.testing.allocator.free(path);
    var state: State = .{ .config_path = path };
    defer state.deinit();
    var input = [_]u8{0} ** 16;
    input[0] = 1;
    std.mem.writeInt(u32, input[10..14], 1, .little);
    std.mem.writeInt(u16, input[14..16], 2, .little);
    var output: [max_bytes]u8 = undefined;
    const page_bytes = try handle(&state, .{}, &input, &output);
    try std.testing.expectEqual(@as(u16, 2), std.mem.readInt(u16, page_bytes[18..20], .little));
    try std.testing.expectEqual(Route.unsupported, state.row(1).?.route);
    try std.testing.expectEqualStrings("next", state.row(2).?.identity.name);
}
