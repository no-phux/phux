//! Dedicated local Phux tool launches. No shell command strings or remote focus
//! inference cross this boundary. The runtime owns spawn/placement receipts.
const std = @import("std");

pub const request_name = "cockpit.local-tools";
pub const max_bytes = 4096;
pub const Kind = enum(u8) { describe = 1, edit_config = 2, add_machine = 3, status = 4, acknowledge = 5 };
pub const Phase = enum(u8) { ready = 0, queued = 1, failed = 2, editor_required = 3, placed = 4, unknown = 5 };
pub const ToolStatus = struct { phase: Phase, message: []const u8 };
pub const Error = error{ InvalidRequest, BufferTooSmall };
pub const Request = struct { kind: Kind, token: u64, destination: []const u8, name: []const u8 };
pub const Reply = struct { phase: Phase, operation_id: u32 = 0, token: u64 = 0, target: []const u8 = "", message: []const u8 = "" };

test "local tool operation receipt status and acknowledgement cross the binary boundary" {
    var request = [_]u8{ 1, 4, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0 };
    try std.testing.expect((decode(&request) catch null) != null);
    request[1] = 5;
    try std.testing.expect((decode(&request) catch null) != null);
}

pub const State = struct {
    const Capture = struct { token: u64, epoch: u64, platform_id: u64, consumed: bool = false, operation_id: u32 = 0 };
    const Receipt = struct { operation_id: u32, target: []u8 };
    const Acknowledged = struct { token: u64, bytes: [max_bytes]u8, len: usize };
    captures: std.AutoHashMapUnmanaged(usize, Capture) = .empty,
    // Read-only receipt capabilities survive another describe and window closure.
    // Explicit acknowledgement retires them; never evict an unreported outcome.
    receipts: std.AutoHashMapUnmanaged(u64, Receipt) = .empty,
    acknowledged: [16]?Acknowledged = @splat(null),
    next_ack: usize = 0,
    next_token: u64 = 1,

    fn rememberAcknowledgement(self: *State, token: u64, encoded: []const u8) void {
        const slot = &self.acknowledged[self.next_ack];
        slot.* = .{ .token = token, .bytes = undefined, .len = encoded.len };
        @memcpy(slot.*.?.bytes[0..encoded.len], encoded);
        self.next_ack = (self.next_ack + 1) % self.acknowledged.len;
    }

    fn repeatedAcknowledgement(self: *State, request: Request, out: []u8) Error![]const u8 {
        for (&self.acknowledged) |*slot| {
            const ack = if (slot.*) |*value| value else continue;
            if (request.kind != .acknowledge or ack.token != request.token) continue;
            if (out.len < ack.len) return error.BufferTooSmall;
            @memcpy(out[0..ack.len], ack.bytes[0..ack.len]);
            return out[0..ack.len];
        }
        return encode(.{ .phase = .failed, .token = request.token, .message = "This launch receipt is no longer available." }, out);
    }

    pub fn deinit(self: *State) void {
        var receipts = self.receipts.valueIterator();
        while (receipts.next()) |receipt| std.heap.page_allocator.free(receipt.target);
        self.receipts.deinit(std.heap.page_allocator);
        self.captures.deinit(std.heap.page_allocator);
        self.* = .{};
    }

    fn capture(self: *State, model: anytype) !u64 {
        const window = model.active_window;
        if (!model.windowOpen(window)) return error.InvalidWindow;
        const workspace = model.wsAt(window) orelse return error.InvalidWindow;
        const token = self.next_token;
        self.next_token = std.math.add(u64, token, 1) catch return error.CaptureExhausted;
        try self.captures.put(std.heap.page_allocator, window, .{ .token = token, .epoch = model.window_epochs[window], .platform_id = workspace.window_id });
        return token;
    }

    fn resolve(self: *State, model: anytype, token: u64) !*Capture {
        var entries = self.captures.iterator();
        while (entries.next()) |entry| {
            if (entry.value_ptr.token != token) continue;
            const window = entry.key_ptr.*;
            const value = entry.value_ptr;
            if (value.consumed or !model.windowOpen(window)) return error.InvalidWindow;
            if (model.window_epochs[window] != value.epoch) return error.InvalidWindow;
            const workspace = model.wsAt(window) orelse return error.InvalidWindow;
            if (workspace.window_id != value.platform_id) return error.InvalidWindow;
            return value;
        }
        return error.InvalidWindow;
    }
};

fn environment(name: [*:0]const u8) ?[]const u8 {
    return if (std.c.getenv(name)) |value| std.mem.span(value) else null;
}

/// v1, kind:u8, opaque capture token:u64le, destination:u8+bytes, name:u8+bytes.
pub fn decode(bytes: []const u8) Error!Request {
    if (bytes.len < 12 or bytes[0] != 1) return error.InvalidRequest;
    const kind = std.enums.fromInt(Kind, bytes[1]) orelse return error.InvalidRequest;
    const end = 11 + @as(usize, bytes[10]);
    if (end >= bytes.len) return error.InvalidRequest;
    if (end + 1 + @as(usize, bytes[end]) != bytes.len) return error.InvalidRequest;
    const request: Request = .{ .kind = kind, .token = std.mem.readInt(u64, bytes[2..10], .little), .destination = bytes[11..end], .name = bytes[end + 1 ..] };
    try validateRequest(request);
    return request;
}

fn validateRequest(request: Request) Error!void {
    if (request.kind != .add_machine) {
        if (request.destination.len != 0 or request.name.len != 0) return error.InvalidRequest;
        return;
    }
    try validateDestination(request.destination);
    if (!std.unicode.utf8ValidateSlice(request.name)) return error.InvalidRequest;
    for (request.name) |ch| {
        if (ch < 32 or ch == 127) return error.InvalidRequest;
    }
}

fn validateDestination(destination: []const u8) Error!void {
    if (destination.len == 0) return error.InvalidRequest;
    if (destination[0] == '-') return error.InvalidRequest;
    for (destination) |ch| {
        if (!std.ascii.isAlphanumeric(ch) and std.mem.indexOfScalar(u8, "@._-:", ch) == null) return error.InvalidRequest;
    }
}

/// v1, phase:u8, operation:u32le, token:u64le, target:u16le+bytes, message:u16le+bytes.
pub fn encode(reply: Reply, out: []u8) Error![]const u8 {
    const size = 18 + reply.target.len + reply.message.len;
    if (size > out.len or size > max_bytes) return error.BufferTooSmall;
    out[0] = 1;
    out[1] = @intFromEnum(reply.phase);
    std.mem.writeInt(u32, out[2..6], reply.operation_id, .little);
    std.mem.writeInt(u64, out[6..14], reply.token, .little);
    std.mem.writeInt(u16, out[14..16], @intCast(reply.target.len), .little);
    @memcpy(out[16..][0..reply.target.len], reply.target);
    const end = 16 + reply.target.len;
    std.mem.writeInt(u16, out[end..][0..2], @intCast(reply.message.len), .little);
    @memcpy(out[end + 2 ..][0..reply.message.len], reply.message);
    return out[0..size];
}

pub fn handle(state: *State, engine: anytype, fx: anytype, preview_dirty: bool, payload: []const u8, out: []u8) Error![]const u8 {
    const request = try decode(payload);
    if (request.kind == .status or request.kind == .acknowledge) return handleStatus(state, engine, request, out);
    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();
    const reply = capturedPerform(state, engine, fx, preview_dirty, request, arena.allocator()) catch |err| Reply{
        .phase = .failed,
        .token = request.token,
        .target = if (request.kind == .add_machine) request.destination else engine.model.config_file.path(),
        .message = failureMessage(err),
    };
    return encode(reply, out);
}

fn handleStatus(state: *State, engine: anytype, request: Request, out: []u8) Error![]const u8 {
    const receipt = state.receipts.get(request.token) orelse return state.repeatedAcknowledgement(request, out);
    const status = engine.localToolStatus(receipt.operation_id);
    const encoded = try encode(.{ .phase = status.phase, .operation_id = receipt.operation_id, .token = request.token, .target = receipt.target, .message = status.message }, out);
    if (request.kind == .acknowledge and status.phase != .queued) {
        _ = engine.acknowledgeLocalTool(receipt.operation_id);
        state.rememberAcknowledgement(request.token, encoded);
        _ = state.receipts.remove(request.token);
        std.heap.page_allocator.free(receipt.target);
    }
    return encoded;
}

fn capturedPerform(state: *State, engine: anytype, fx: anytype, preview_dirty: bool, request: Request, gpa: std.mem.Allocator) !Reply {
    const token = if (request.kind == .describe) try state.capture(engine.model) else request.token;
    const captured = try state.resolve(engine.model, token);
    if (request.kind != .describe) return capturedLaunch(state, engine, fx, preview_dirty, request, captured, gpa);
    var reply = perform(engine, fx, preview_dirty, request, captured.platform_id, gpa) catch |err| Reply{
        .phase = .failed,
        .target = engine.model.config_file.path(),
        .message = failureMessage(err),
    };
    reply.token = token;
    return reply;
}

fn capturedLaunch(state: *State, engine: anytype, fx: anytype, preview_dirty: bool, request: Request, captured: *State.Capture, gpa: std.mem.Allocator) !Reply {
    if (state.receipts.count() == 16) return error.OperationCapacity;
    try state.receipts.ensureUnusedCapacity(std.heap.page_allocator, 1);
    const target = if (request.kind == .add_machine) request.destination else engine.model.config_file.path();
    const owned = try std.heap.page_allocator.dupe(u8, target);
    var retained = false;
    defer if (!retained) std.heap.page_allocator.free(owned);
    var reply = perform(engine, fx, preview_dirty, request, captured.platform_id, gpa) catch |err| Reply{
        .phase = .failed,
        .target = if (request.kind == .add_machine) request.destination else engine.model.config_file.path(),
        .message = failureMessage(err),
    };
    reply.token = request.token;
    if (reply.phase == .queued) {
        captured.consumed = true;
        captured.operation_id = reply.operation_id;
        state.receipts.putAssumeCapacity(request.token, .{ .operation_id = reply.operation_id, .target = owned });
        retained = true;
    }
    return reply;
}

fn perform(engine: anytype, fx: anytype, preview_dirty: bool, request: Request, window_id: u64, gpa: std.mem.Allocator) !Reply {
    if (request.kind == .add_machine) return addMachine(engine, fx, request, window_id, gpa);
    const path = engine.model.config_file.path();
    if (!std.fs.path.isAbsolute(path)) return error.ConfigPathUnavailable;
    const argv = editorArgv(gpa, engine.model.provider.io, engine.model.config.editorCommand(), environment("VISUAL"), environment("EDITOR"), environment("PATH"), path) catch return .{
        .phase = .editor_required,
        .target = path,
        .message = "Choose an executable in Settings > Preferred editor (arguments and quoted paths are supported), then Retry.",
    };
    if (request.kind == .describe) return .{ .phase = .ready, .target = path, .message = try describeEditor(gpa, argv) };
    if (preview_dirty) return error.SettingsPreviewPending;
    try ensureConfigFile(engine.model.provider.io, path);
    const operation = try engine.launchLocalTool(fx, window_id, argv, "Edit Configuration — This Mac");
    return .{ .phase = .queued, .operation_id = operation, .target = path, .message = "Opening a dedicated local Phux editor terminal" };
}

fn describeEditor(gpa: std.mem.Allocator, argv: []const []const u8) ![]const u8 {
    const text = try std.mem.join(gpa, " | ", argv[0 .. argv.len - 1]);
    // This is display evidence only. Full structured argv remains unchanged.
    var end = @min(text.len, 1024);
    while (end < text.len and end > 0 and (text[end] & 0xc0) == 0x80) end -= 1;
    return text[0..end];
}

fn addMachine(engine: anytype, fx: anytype, request: Request, window_id: u64, gpa: std.mem.Allocator) !Reply {
    var cli_buffer: [4096]u8 = undefined;
    const cli = try engine.localToolCli(&cli_buffer);
    const argv = try enrollmentArgv(gpa, cli, request.destination, request.name);
    const operation = try engine.launchLocalTool(fx, window_id, argv, "Add Machine — This Mac");
    return .{ .phase = .queued, .operation_id = operation, .target = request.destination, .message = "Complete Phux setup, then return to Machines and Recheck. Setup exit does not prove connectivity." };
}

fn failureMessage(err: anyerror) []const u8 {
    return switch (err) {
        error.ConfigPathUnavailable => "Configuration destination is unresolved. Set PHUX_COCKPIT_CONFIG to an absolute writable path and Retry.",
        error.SettingsPreviewPending => "Save or Discard pending Settings changes before opening the editor, or Cancel to continue editing.",
        error.AccessDenied, error.ReadOnlyFileSystem => "The local configuration destination is not writable. Check its permissions and Retry.",
        error.LocalRuntimeNotReady => "Local Phux is not ready. Return to This Mac, Retry the connection or Repair Installation, then try again.",
        error.InvalidWindow => "The invoking window has closed. Open this action again from an existing window.",
        error.OperationCapacity => "Local tool receipts are still pending. Check their outcomes in This Mac before opening another tool.",
        else => "Could not open the dedicated local Phux terminal. Check This Mac connection and Retry; current work is intact.",
    };
}

/// Existing content, including invalid/unknown future content, is never rewritten.
/// An empty file is a valid Cockpit configuration. Creation is exclusive.
pub fn ensureConfigFile(io: std.Io, path: []const u8) !void {
    if (!std.fs.path.isAbsolute(path)) return error.ConfigPathUnavailable;
    const dir = std.Io.Dir.cwd();
    const parent = std.fs.path.dirname(path) orelse return error.ConfigPathUnavailable;
    try dir.createDirPath(io, parent);
    const file = dir.createFile(io, path, .{ .exclusive = true, .truncate = false, .permissions = .fromMode(0o600) }) catch |err| switch (err) {
        error.PathAlreadyExists => return existingConfigFile(io, path),
        else => return err,
    };
    file.close(io);
}

fn existingConfigFile(io: std.Io, path: []const u8) !void {
    const dir = std.Io.Dir.cwd();
    const stat = try dir.statFile(io, path, .{});
    if (stat.kind != .file) return error.ConfigPathUnavailable;
    try dir.access(io, path, .{ .read = true, .write = true });
}

pub fn enrollmentArgv(gpa: std.mem.Allocator, cli: []const u8, destination: []const u8, name: []const u8) ![]const []const u8 {
    try validateRequest(.{ .kind = .add_machine, .token = 0, .destination = destination, .name = name });
    if (!std.fs.path.isAbsolute(cli)) return error.LocalRuntimeNotReady;
    // host enroll is intentionally socketless. The runtime pins the PTY to the
    // selected local socket; this command uses the CLI's existing SSH trust flow.
    if (name.len == 0) return gpa.dupe([]const u8, &.{ cli, "host", "enroll", "--", destination });
    return gpa.dupe([]const u8, &.{ cli, "host", "enroll", "--name", name, "--", destination });
}

pub fn editorArgv(gpa: std.mem.Allocator, io: std.Io, explicit: []const u8, visual: ?[]const u8, editor: ?[]const u8, path_env: ?[]const u8, config: []const u8) ![]const []const u8 {
    // The resolved absolute config path cannot become an editor option.
    if (!std.fs.path.isAbsolute(config)) return error.ConfigPathUnavailable;
    for ([_]?[]const u8{ explicit, visual, editor }) |candidate| {
        const choice = candidate orelse continue;
        return usableEditorArgv(gpa, io, choice, path_env, config) catch |err| {
            if (err == error.OutOfMemory) return err;
            continue;
        };
    }
    return error.EditorRequired;
}

fn usableEditorArgv(gpa: std.mem.Allocator, io: std.Io, choice: []const u8, path_env: ?[]const u8, config: []const u8) ![]const []const u8 {
    var arguments: std.ArrayList([]const u8) = .empty;
    try parseCommand(gpa, choice, &arguments);
    if (arguments.items.len == 0) return error.EditorRequired;
    arguments.items[0] = try resolveExecutable(gpa, io, arguments.items[0], path_env);
    try arguments.append(gpa, config);
    return arguments.toOwnedSlice(gpa);
}

fn executableExists(io: std.Io, path: []const u8) bool {
    std.Io.Dir.cwd().access(io, path, .{ .execute = true }) catch return false;
    const stat = std.Io.Dir.cwd().statFile(io, path, .{}) catch return false;
    return stat.kind == .file;
}

fn resolveExecutable(gpa: std.mem.Allocator, io: std.Io, name: []const u8, path_env: ?[]const u8) ![]const u8 {
    if (std.fs.path.isAbsolute(name)) {
        if (executableExists(io, name)) return name;
        return error.EditorRequired;
    }
    if (std.mem.indexOfScalar(u8, name, '/') != null) return error.EditorRequired;
    const search = path_env orelse "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";
    var directories = std.mem.splitScalar(u8, search, ':');
    while (directories.next()) |directory| {
        if (!std.fs.path.isAbsolute(directory)) continue;
        const path = try std.fs.path.join(gpa, &.{ directory, name });
        if (executableExists(io, path)) return path;
    }
    return error.EditorRequired;
}

/// Shell-like quoting only; no expansion, substitutions, pipelines or execution.
pub fn parseCommand(gpa: std.mem.Allocator, input: []const u8, args: *std.ArrayList([]const u8)) !void {
    if (input.len > 4096) return error.EditorCommandTooLarge;
    var parser: CommandParser = .{ .input = input };
    while (try parser.word(gpa)) |word| {
        if (args.items.len == 63) return error.EditorCommandTooLarge;
        try args.append(gpa, word);
    }
}

const CommandParser = struct {
    input: []const u8,
    index: usize = 0,
    quote: u8 = 0,

    fn word(self: *CommandParser, gpa: std.mem.Allocator) !?[]const u8 {
        self.skipWhitespace();
        if (self.index == self.input.len) return null;
        var result: std.ArrayList(u8) = .empty;
        while (self.index < self.input.len) {
            const ch = self.input[self.index];
            self.index += 1;
            try validateCharacter(ch);
            if (self.quote == 0 and std.ascii.isWhitespace(ch)) break;
            try self.character(gpa, ch, &result);
        }
        if (self.quote != 0) return error.InvalidEditorCommand;
        return try result.toOwnedSlice(gpa);
    }

    fn skipWhitespace(self: *CommandParser) void {
        while (self.index < self.input.len and std.ascii.isWhitespace(self.input[self.index])) self.index += 1;
    }

    fn character(self: *CommandParser, gpa: std.mem.Allocator, ch: u8, result: *std.ArrayList(u8)) !void {
        if (ch == self.quote) {
            self.quote = 0;
            return;
        }
        if (self.quote == 0 and (ch == '\'' or ch == '"')) {
            self.quote = ch;
            return;
        }
        if (ch == '\\' and self.quote != '\'') {
            return self.escape(gpa, result);
        }
        try result.append(gpa, ch);
    }

    fn escape(self: *CommandParser, gpa: std.mem.Allocator, result: *std.ArrayList(u8)) !void {
        if (self.index == self.input.len) return error.InvalidEditorCommand;
        const ch = self.input[self.index];
        try validateCharacter(ch);
        self.index += 1;
        if (self.quote == '"' and std.mem.indexOfScalar(u8, "\\\"$`", ch) == null) try result.append(gpa, '\\');
        try result.append(gpa, ch);
    }
};

fn validateCharacter(ch: u8) !void {
    if (ch == 0 or ch == '\n' or ch == '\r') return error.InvalidEditorCommand;
}

test "editor argv preserves quoted paths and arguments without evaluating shell text" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    var args: std.ArrayList([]const u8) = .empty;
    try parseCommand(arena.allocator(), "'/Applications/My Editor.app/editor' --wait \"a b\" '$HOME;echo hacked' empty\\ word ''", &args);
    const expected = [_][]const u8{ "/Applications/My Editor.app/editor", "--wait", "a b", "$HOME;echo hacked", "empty word", "" };
    try std.testing.expectEqual(expected.len, args.items.len);
    for (expected, args.items) |want, actual| try std.testing.expectEqualStrings(want, actual);
    try std.testing.expectError(error.InvalidEditorCommand, parseCommand(arena.allocator(), "vim 'unfinished", &args));
}

test "editor precedence Finder fallback and missing choice are explicit" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const gpa = arena.allocator();
    const argv = try editorArgv(gpa, std.testing.io, "/usr/bin/true --wait", "/bad/visual", "/bad/editor", null, "/fixture/config with spaces");
    try std.testing.expectEqualStrings("/usr/bin/true", argv[0]);
    try std.testing.expectEqualStrings("--wait", argv[1]);
    try std.testing.expectEqualStrings("/fixture/config with spaces", argv[2]);
    const visual = try editorArgv(gpa, std.testing.io, "", "true", "/bad/editor", null, "/config");
    try std.testing.expectEqualStrings("/usr/bin/true", visual[0]);
    try std.testing.expectError(error.EditorRequired, editorArgv(gpa, std.testing.io, "", null, null, null, "/config"));
    const fallback = try editorArgv(gpa, std.testing.io, "/does/not/exist", "/usr/bin/true", null, null, "/config");
    try std.testing.expectEqualStrings("/usr/bin/true", fallback[0]);
}

test "editor selection skips unusable configured and VISUAL choices before EDITOR" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    for ([_][]const u8{ "/missing/visual", "'unfinished quote", "   \t", "" }) |visual| {
        const argv = try editorArgv(arena.allocator(), std.testing.io, "/missing/configured-editor", visual, "/usr/bin/true --wait", null, "/config with spaces");
        try std.testing.expectEqualStrings("/usr/bin/true", argv[0]);
        try std.testing.expectEqualStrings("--wait", argv[1]);
        try std.testing.expectEqualStrings("/config with spaces", argv[2]);
    }
}

test "config editing creates only isolated missing file and preserves unknown existing bytes" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const root = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(root);
    const path = try std.fs.path.join(std.testing.allocator, &.{ root, "config with spaces" });
    defer std.testing.allocator.free(path);
    try ensureConfigFile(std.testing.io, path);
    const original = "# keep comments\nfuture-config-format=unknown\nmalformed line\n";
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config with spaces", .data = original });
    try ensureConfigFile(std.testing.io, path);
    const actual = try tmp.dir.readFileAlloc(std.testing.io, "config with spaces", std.testing.allocator, .limited(4096));
    defer std.testing.allocator.free(actual);
    try std.testing.expectEqualStrings(original, actual);
    try std.testing.expectError(error.ConfigPathUnavailable, ensureConfigFile(std.testing.io, root));
}

test "enrollment is structured argv on existing CLI trust path and never executed by test" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const argv = try enrollmentArgv(arena.allocator(), "/bundle with spaces/phux", "me@host", "Studio Mac");
    const expected = [_][]const u8{ "/bundle with spaces/phux", "host", "enroll", "--name", "Studio Mac", "--", "me@host" };
    try std.testing.expectEqual(expected.len, argv.len);
    for (expected, argv) |want, actual| try std.testing.expectEqualStrings(want, actual);
    try std.testing.expectError(error.InvalidRequest, enrollmentArgv(arena.allocator(), "/phux", "-oProxyCommand=bad", ""));
    try std.testing.expectError(error.InvalidRequest, enrollmentArgv(arena.allocator(), "/phux", "host;echo bad", ""));
}

test "local tools binary seam preserves captured window and rejects trailing fields" {
    var bytes = [_]u8{ 1, 2, 77, 0, 0, 0, 0, 0, 0, 0, 0, 0 };
    const request = try decode(&bytes);
    try std.testing.expectEqual(@as(u64, 77), request.token);
    try std.testing.expectEqual(Kind.edit_config, request.kind);
    bytes[11] = 1;
    try std.testing.expectError(error.InvalidRequest, decode(&bytes));
    var output: [max_bytes]u8 = undefined;
    const encoded = try encode(.{ .phase = .queued, .operation_id = 42, .target = "/config", .message = "local" }, &output);
    try std.testing.expectEqual(@as(u32, 42), std.mem.readInt(u32, encoded[2..6], .little));
    try std.testing.expectEqualStrings("/config", encoded[16..23]);
}

const TestEngine = struct {
    const Config = struct {
        pub fn editorCommand(_: Config) []const u8 {
            return "/usr/bin/true --wait";
        }
    };
    const ConfigFile = struct {
        value: []const u8,
        pub fn path(self: ConfigFile) []const u8 {
            return self.value;
        }
    };
    const Model = struct {
        const Workspace = struct { window_id: u64 };
        provider: struct { io: std.Io = std.testing.io } = .{},
        config: Config = .{},
        config_file: ConfigFile,
        active_window: usize = 0,
        window_epochs: [2]u64 = .{ 1, 1 },
        workspace: Workspace = .{ .window_id = 77 },
        second_workspace: Workspace = .{ .window_id = 88 },
        pub fn windowOpen(_: *@This(), index: usize) bool {
            return index < 2;
        }
        pub fn wsAt(self: *@This(), index: usize) ?@TypeOf(&self.workspace) {
            return switch (index) {
                0 => &self.workspace,
                1 => &self.second_workspace,
                else => null,
            };
        }
    };
    model: *Model,
    calls: usize = 0,
    window: u64 = 0,
    executable: [100]u8 = undefined,
    executable_len: usize = 0,
    target: [1024]u8 = undefined,
    target_len: usize = 0,
    tool_status: ToolStatus = .{ .phase = .queued, .message = "pending" },
    acknowledgements: usize = 0,

    pub fn localToolStatus(self: *TestEngine, _: u32) ToolStatus {
        return self.tool_status;
    }

    pub fn acknowledgeLocalTool(self: *TestEngine, _: u32) bool {
        self.acknowledgements += 1;
        return true;
    }

    pub fn launchLocalTool(self: *TestEngine, _: void, window: u64, argv: []const []const u8, _: []const u8) !u32 {
        self.calls += 1;
        self.window = window;
        self.executable_len = argv[0].len;
        @memcpy(self.executable[0..self.executable_len], argv[0]);
        const target = argv[argv.len - 1];
        self.target_len = target.len;
        @memcpy(self.target[0..self.target_len], target);
        return 42;
    }

    pub fn localToolCli(_: *TestEngine, buffer: []u8) ![]const u8 {
        const cli = "/fixture/phux";
        @memcpy(buffer[0..cli.len], cli);
        return buffer[0..cli.len];
    }
};

test "native edit request refuses dirty preview before file creation and correlates local spawn" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const root = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(root);
    const path = try std.fs.path.join(std.testing.allocator, &.{ root, "new config" });
    defer std.testing.allocator.free(path);
    var model: TestEngine.Model = .{ .config_file = .{ .value = path } };
    var engine: TestEngine = .{ .model = &model };
    var state: State = .{};
    defer state.deinit();
    var request = [_]u8{ 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 };
    var output: [max_bytes]u8 = undefined;
    const described = try handle(&state, &engine, {}, true, &request, &output);
    const token = std.mem.readInt(u64, described[6..14], .little);
    request[1] = 2;
    std.mem.writeInt(u64, request[2..10], token, .little);
    const denied = try handle(&state, &engine, {}, true, &request, &output);
    try std.testing.expectEqual(@intFromEnum(Phase.failed), denied[1]);
    try std.testing.expectEqual(@as(usize, 0), engine.calls);
    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, "new config", .{}));
    const queued = try handle(&state, &engine, {}, false, &request, &output);
    try std.testing.expectEqual(@intFromEnum(Phase.queued), queued[1]);
    try std.testing.expectEqual(@as(u32, 42), std.mem.readInt(u32, queued[2..6], .little));
    try std.testing.expectEqual(@as(usize, 1), engine.calls);
    try std.testing.expectEqual(@as(u64, 77), engine.window);
    try std.testing.expectEqualStrings("/usr/bin/true", engine.executable[0..engine.executable_len]);
    try std.testing.expectEqualStrings(path, engine.target[0..engine.target_len]);
    const duplicate = try handle(&state, &engine, {}, false, &request, &output);
    try std.testing.expectEqual(@intFromEnum(Phase.failed), duplicate[1]);
    try std.testing.expectEqual(@as(usize, 1), engine.calls);
    request[1] = 1;
    const second = try handle(&state, &engine, {}, false, &request, &output);
    const second_token = std.mem.readInt(u64, second[6..14], .little);
    model.window_epochs[0] += 1;
    request[1] = 2;
    std.mem.writeInt(u64, request[2..10], second_token, .little);
    const stale = try handle(&state, &engine, {}, false, &request, &output);
    try std.testing.expectEqual(@intFromEnum(Phase.failed), stale[1]);
    try std.testing.expectEqual(@as(usize, 1), engine.calls);
}

test "setup capture survives focus change and cannot be replayed" {
    var model: TestEngine.Model = .{ .config_file = .{ .value = "/fixture/config" } };
    var engine: TestEngine = .{ .model = &model };
    var state: State = .{};
    defer state.deinit();
    const first = try state.capture(&model);
    model.active_window = 1;
    const second = try state.capture(&model);
    try std.testing.expect(first != second);
    var request = [_]u8{ 1, 3, 0, 0, 0, 0, 0, 0, 0, 0, 4, 'h', 'o', 's', 't', 0 };
    std.mem.writeInt(u64, request[2..10], first, .little);
    var output: [max_bytes]u8 = undefined;
    const queued = try handle(&state, &engine, {}, false, &request, &output);
    try std.testing.expectEqual(@intFromEnum(Phase.queued), queued[1]);
    try std.testing.expectEqual(@as(u64, 77), engine.window);
    try std.testing.expectEqualStrings("/fixture/phux", engine.executable[0..engine.executable_len]);
    const replayed = try handle(&state, &engine, {}, false, &request, &output);
    try std.testing.expectEqual(@intFromEnum(Phase.failed), replayed[1]);
    try std.testing.expectEqual(@as(usize, 1), engine.calls);
    std.mem.writeInt(u64, request[2..10], second, .little);
    const other = try handle(&state, &engine, {}, false, &request, &output);
    try std.testing.expectEqual(@intFromEnum(Phase.queued), other[1]);
    try std.testing.expectEqual(@as(u64, 88), engine.window);
    try std.testing.expectEqual(@as(usize, 2), engine.calls);
}

test "tool receipt survives new describe and closed window until terminal acknowledgement" {
    var model: TestEngine.Model = .{ .config_file = .{ .value = "/fixture/config" } };
    var engine: TestEngine = .{ .model = &model };
    var state: State = .{};
    defer state.deinit();
    var output: [max_bytes]u8 = undefined;
    const describe = [_]u8{ 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 };
    const initial = try handle(&state, &engine, {}, false, &describe, &output);
    const token = std.mem.readInt(u64, initial[6..14], .little);
    var poll = [_]u8{ 1, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 };
    std.mem.writeInt(u64, poll[2..10], token, .little);
    try std.testing.expectEqual(@intFromEnum(Phase.failed), (try handle(&state, &engine, {}, false, &poll, &output))[1]);
    var setup = [_]u8{ 1, 3, 0, 0, 0, 0, 0, 0, 0, 0, 4, 'h', 'o', 's', 't', 0 };
    std.mem.writeInt(u64, setup[2..10], token, .little);
    try std.testing.expectEqual(@intFromEnum(Phase.queued), (try handle(&state, &engine, {}, false, &setup, &output))[1]);
    const next = try handle(&state, &engine, {}, false, &describe, &output);
    try std.testing.expect(token != std.mem.readInt(u64, next[6..14], .little));
    model.window_epochs[0] += 1;
    poll[1] = 5;
    try std.testing.expectEqual(@intFromEnum(Phase.queued), (try handle(&state, &engine, {}, false, &poll, &output))[1]);
    try std.testing.expectEqual(@as(usize, 0), engine.acknowledgements);
    engine.tool_status = .{ .phase = .unknown, .message = "Check This Mac; never replay" };
    poll[1] = 4;
    const final = try handle(&state, &engine, {}, false, &poll, &output);
    try std.testing.expectEqual(@intFromEnum(Phase.unknown), final[1]);
    try std.testing.expectEqual(@as(u32, 42), std.mem.readInt(u32, final[2..6], .little));
    try std.testing.expectEqualStrings("host", final[16..20]);
    try std.testing.expectEqual(@as(usize, 1), engine.calls);
    poll[1] = 5;
    const ack = try handle(&state, &engine, {}, false, &poll, &output);
    try std.testing.expectEqual(@intFromEnum(Phase.unknown), ack[1]);
    try std.testing.expectEqualStrings("host", ack[16..20]);
    try std.testing.expectEqual(@as(usize, 1), engine.acknowledgements);
    const retried = try handle(&state, &engine, {}, false, &poll, &output);
    try std.testing.expectEqual(@intFromEnum(Phase.unknown), retried[1]);
    try std.testing.expectEqual(@as(u32, 42), std.mem.readInt(u32, retried[2..6], .little));
    try std.testing.expectEqual(@as(usize, 1), engine.acknowledgements);
    poll[1] = 4;
    try std.testing.expectEqual(@intFromEnum(Phase.failed), (try handle(&state, &engine, {}, false, &poll, &output))[1]);
}
