//! Reversible appearance edits. Preview owns no execution or topology authority.
const std = @import("std");
const model_module = @import("../model.zig");
const config = @import("../../config/config.zig");
const themes = @import("../../config/theme.zig");
const projection = @import("workspace_projection.zig");
const Model = model_module.Model;
const TabPlacement = @import("../topology.zig").TabPlacement;
const local = @import("../../providers/local/provider.zig");
const TerminalRef = model_module.TerminalRef;

pub const request_name = "cockpit.appearance";
pub const max_bytes = 8192;
pub const Outcome = enum(u8) { preview, saved, canceled, refused, no_destination, invalid, conflict, malformed };

// Wire IDs are append-only; display descriptions live in settings.ts.
pub const fields = .{
    .{ "font-family", "font_family" },                             .{ "font-size", "font_size" },
    .{ "theme", "theme" },                                         .{ "minimum-contrast", "minimum_contrast" },
    .{ "cursor-style", "cursor_style" },                           .{ "cursor-style-blink", "cursor_style_blink" },
    .{ "scrollback-limit", "scrollback_bytes" },                   .{ "shell", "shell" },
    .{ "inherit-working-directory", "inherit_working_directory" }, .{ "tab-placement", "tab_placement" },
    .{ "editor", "editor" },
};

const Values = struct {
    config: config.Config,
    font_offset: f32,
    placement: TabPlacement,

    fn capture(model: *const Model) Values {
        return .{ .config = model.config, .font_offset = model.font_size_offset, .placement = model.tab_placement };
    }

    fn restore(self: Values, model: *Model) void {
        inline for (fields) |pair| @field(model.config, pair[1]) = @field(self.config, pair[1]);
        model.config.follow_system_theme = self.config.follow_system_theme;
        model.config.keybindings = self.config.keybindings;
        model.font_size_offset = self.font_offset;
        applyLocalDefaults(model);
        model.tab_placement = self.placement;
        model.appearance_committed_placement = null;
    }

    fn themeChanged(self: Values, model: *const Model) bool {
        if (self.config.follow_system_theme != model.config.follow_system_theme) return true;
        if (self.config.follow_system_theme) return false;
        return !std.mem.eql(u8, self.config.theme.slice(), model.config.theme.slice());
    }

    fn fontChanged(self: Values, model: *const Model) bool {
        return self.config.font_size != model.config.font_size or self.font_offset != model.font_size_offset;
    }

    fn changed(self: Values, model: *const Model) bool {
        inline for (fields, 0..) |_, id| if (fieldChanged(self, model, id)) return true;
        return !std.meta.eql(self.config.keybindings, model.config.keybindings);
    }
};

pub const State = struct {
    initial: ?Values = null,
    outcome: Outcome = .preview,
    source_hash: ?u64 = null,
    read_error: bool = false,
    /// The host updates this on every appearance event, even outside Settings.
    system_scheme: themes.ColorScheme = .dark,
    /// Parent binds this to the registry derived from the shipping app manifest.
    binding_registry: ?*const config.keybindings_module.Registry = null,

    pub fn setSystemAppearance(self: *State, model: *Model, scheme: themes.ColorScheme) bool {
        self.system_scheme = scheme;
        return model.config.adoptSystemTheme(scheme);
    }

    pub fn hasPendingChanges(self: *const State, model: *const Model) bool {
        const before = self.initial orelse return false;
        return before.changed(model);
    }

    /// The host installation is part of accepting a Settings transition.
    /// Save installs before writing, because a durable file cannot be rolled
    /// back by restoring an in-memory snapshot. Other transitions are reversible.
    /// The installer owns platform rollback and reports unknown installation
    /// state if that rollback fails; this wrapper restores client state only.
    pub fn applyWithBindings(self: *State, model: *Model, bytes: []const u8, installer: anytype) !void {
        if (isSaveRequest(bytes) and self.initial != null) {
            installer.sync(&model.config.keybindings) catch |err| {
                self.outcome = .refused;
                return err;
            };
            self.apply(model, bytes);
            return;
        }

        const before = TransitionSnapshot.capture(self.*, model);
        self.apply(model, bytes);
        if (!acceptedTransition(self.outcome)) return;
        installer.sync(&model.config.keybindings) catch |err| {
            before.restore(self, model);
            self.outcome = .refused;
            return err;
        };
    }

    /// Requests are synchronous on the app loop; the core admits one at a time.
    /// Begin is idempotent, so reopening cannot overwrite the rollback point.
    pub fn apply(self: *State, model: *Model, bytes: []const u8) void {
        if (bytes.len < 3) {
            self.outcome = .invalid;
            return;
        }
        if (bytes[0] == 2) return self.applySetting(model, bytes);
        if (bytes.len != 3 or bytes[0] != 1) {
            self.outcome = .invalid;
            return;
        }
        if (bytes[1] == 0) {
            if (self.initial == null) {
                self.initial = Values.capture(model);
                model.appearance_committed_placement = model.tab_placement;
                self.read_error = false;
                self.source_hash = sourceHash(model) catch blk: {
                    self.read_error = true;
                    break :blk null;
                };
            }
            self.outcome = .preview;
            return;
        }
        const before = self.initial orelse {
            self.outcome = .invalid;
            return;
        };
        self.outcome = .preview;
        self.edit(model, before, bytes[1], bytes[2]);
    }

    fn applySetting(self: *State, model: *Model, bytes: []const u8) void {
        if (bytes[1] == 10) return self.reload(model);
        if (self.initial == null) {
            self.outcome = .invalid;
            return;
        }
        self.outcome = .preview;
        switch (bytes[1]) {
            8 => self.setValue(model, bytes[2], bytes[3..]),
            9 => self.resetValue(model, bytes[2]),
            else => self.outcome = .invalid,
        }
    }

    fn setValue(self: *State, model: *Model, id: u8, value: []const u8) void {
        const next = editedConfig(model.config, id, value) catch {
            self.outcome = .invalid;
            return;
        };
        model.config = next;
        self.applyEffects(model, id);
    }

    fn resetValue(self: *State, model: *Model, id: u8) void {
        const defaults: config.Config = .{};
        inline for (fields, 0..) |pair, index| {
            if (id == index) {
                @field(model.config, pair[1]) = @field(defaults, pair[1]);
                if (id == 2) model.config.follow_system_theme = false;
                self.applyEffects(model, id);
                return;
            }
        }
        self.outcome = .invalid;
    }

    fn applyEffects(self: *State, model: *Model, id: u8) void {
        if (id == 1) model.font_size_offset = 0;
        if (id == 2) _ = model.config.adoptSystemTheme(self.system_scheme);
        if (id == 9) model.tab_placement = if (model.config.tab_placement == .side) .side else .top;
        applyLocalDefaults(model);
    }

    pub fn reload(self: *State, model: *Model) void {
        if (self.initial != null) {
            self.outcome = .conflict;
            return;
        }
        reloadConfig(model, self.binding_registry) catch |err| {
            self.outcome = if (err == error.MalformedConfig) .malformed else .refused;
            return;
        };
        _ = model.config.adoptSystemTheme(self.system_scheme);
        self.outcome = .saved;
    }

    fn edit(self: *State, model: *Model, before: Values, action: u8, argument: u8) void {
        switch (action) {
            1 => self.previewTheme(model, argument),
            2 => _ = model.stepFontSize(1),
            3 => _ = model.stepFontSize(-1),
            4 => self.previewCursor(model, argument),
            5 => self.previewPlacement(model, argument),
            6 => {
                before.restore(model);
                _ = model.config.adoptSystemTheme(self.system_scheme);
                self.initial = null;
                self.outcome = .canceled;
            },
            7 => self.save(model, before),
            else => self.outcome = .invalid,
        }
    }

    fn previewTheme(self: *State, model: *Model, index: u8) void {
        if (index >= themes.builtins.len) {
            self.outcome = .invalid;
            return;
        }
        _ = model.config.setTheme(themes.builtins[index].name);
    }

    fn previewCursor(self: *State, model: *Model, index: u8) void {
        model.config.cursor_style = std.enums.fromInt(config.CursorStyle, index) orelse {
            self.outcome = .invalid;
            return;
        };
        applyLocalDefaults(model);
    }

    fn previewPlacement(self: *State, model: *Model, index: u8) void {
        model.tab_placement = std.enums.fromInt(TabPlacement, index) orelse {
            self.outcome = .invalid;
            return;
        };
    }

    fn save(self: *State, model: *Model, before: Values) void {
        if (!before.changed(model)) {
            model.appearance_committed_placement = null;
            self.initial = null;
            self.outcome = .saved;
            return;
        }
        if (!model.config_file.enabled()) {
            self.outcome = .no_destination;
            return;
        }
        if (self.read_error) {
            self.outcome = .refused;
            return;
        }
        validateBindings(&model.config, self.binding_registry) catch {
            self.outcome = .invalid;
            return;
        };
        persist(model, before, self.source_hash) catch |err| {
            self.outcome = switch (err) {
                error.ExternalConfigChange => .conflict,
                error.MalformedConfig => .malformed,
                else => .refused,
            };
            return;
        };
        if (before.fontChanged(model)) {
            model.config.font_size = model.fontSize();
            model.font_size_offset = 0;
        }
        model.config.tab_placement = if (model.tab_placement == .side) .side else .top;
        model.appearance_committed_placement = null;
        self.initial = null;
        self.outcome = .saved;
        model.config_write_refused = false;
    }

    pub fn encode(self: *const State, model: *const Model, out: *[max_bytes]u8) []const u8 {
        out[0] = 2;
        out[1] = @intFromBool(self.initial != null);
        out[2] = @intFromEnum(self.outcome);
        out[3] = if (self.initial) |before| @intFromBool(before.changed(model)) else 0;
        out[4] = if (themes.indexOf(model.config.theme.slice())) |index| @intCast(index) else 255;
        out[5] = @intFromEnum(model.config.cursor_style);
        out[6] = @intFromEnum(model.tab_placement);
        out[7] = @intFromBool(model.config.background != null or model.config.foreground != null);
        const font = std.fmt.bufPrint(out[10..48], "{d:.1} pt", .{model.fontSize()}) catch unreachable;
        out[8] = @intCast(font.len);
        const tokens = projection.terminalTokens(model);
        const fg = tokens.colors.text;
        const bg = tokens.colors.background;
        const contrast = std.fmt.bufPrint(out[10 + font.len ..], "{d:.1}:1 text contrast", .{contrastRatio(fg.r, fg.g, fg.b, bg.r, bg.g, bg.b)}) catch unreachable;
        out[9] = @intCast(contrast.len);
        var end: usize = 10 + font.len + contrast.len;
        out[end] = @intFromBool(model.config.follow_system_theme);
        end += 1;
        inline for (fields, 0..) |_, id| {
            var buffer: [config.max_shell_bytes]u8 = undefined;
            const value = fieldValue(model, id, &buffer);
            out[end] = @intCast(id);
            std.mem.writeInt(u16, out[end + 1 ..][0..2], @intCast(value.len), .little);
            @memcpy(out[end + 3 ..][0..value.len], value);
            end += 3 + value.len;
        }
        return out[0..end];
    }
};

const TransitionSnapshot = struct {
    state: State,
    config: config.Config,
    font_offset: f32,
    placement: TabPlacement,
    committed_placement: ?TabPlacement,
    write_refused: bool,

    fn capture(state: State, model: *const Model) TransitionSnapshot {
        return .{
            .state = state,
            .config = model.config,
            .font_offset = model.font_size_offset,
            .placement = model.tab_placement,
            .committed_placement = model.appearance_committed_placement,
            .write_refused = model.config_write_refused,
        };
    }

    fn restore(self: TransitionSnapshot, state: *State, model: *Model) void {
        state.* = self.state;
        model.config = self.config;
        model.font_size_offset = self.font_offset;
        model.tab_placement = self.placement;
        model.appearance_committed_placement = self.committed_placement;
        model.config_write_refused = self.write_refused;
        applyLocalDefaults(model);
    }
};

fn isSaveRequest(bytes: []const u8) bool {
    if (bytes.len != 3) return false;
    return bytes[0] == 1 and bytes[1] == 7;
}

fn acceptedTransition(outcome: Outcome) bool {
    return switch (outcome) {
        .preview, .saved, .canceled => true,
        else => false,
    };
}

fn linear(value: f32) f32 {
    return if (value <= 0.04045) value / 12.92 else std.math.pow(f32, (value + 0.055) / 1.055, 2.4);
}

fn applyLocalDefaults(model: *Model) void {
    model.provider.max_scrollback_bytes = @intCast(model.config.scrollback_bytes);
    // Only newly created panes read defaultArgv. Running processes are intact.
    _ = model.provider.shell_command.set(model.config.shell.slice());
    var refs: [local.max_terminals]TerminalRef = undefined;
    const count = model.provider.terminalRefs(&refs);
    for (refs[0..count]) |ref| {
        const pane = model.provider.terminal(ref) orelse continue;
        pane.session.term.setDefaultCursorStyle(switch (model.config.cursor_style) {
            .block => .block,
            .bar => .bar,
            .underline => .underline,
        });
        pane.session.term.setDefaultCursorBlink(model.config.cursor_style_blink);
    }
}

/// Creation hook: keep the established directory quoting, then substitute the
/// explicitly configured command. Storage belongs to the model until spawn.
pub fn scratchArgvIn(model: *Model, cwd: []const u8, slot: usize) []const []const u8 {
    const out = &model.cwd_argv[slot];
    const argv = local.paneArgvIn(cwd, out);
    const shell = model.config.shell.slice();
    if (shell.len == 0) return argv;
    const command_index = argv.len - 1;
    const command = argv[command_index];
    if (command.ptr != &out.command) return model.provider.defaultArgv();
    const marker = std.mem.lastIndexOf(u8, command, "; exec ") orelse return model.provider.defaultArgv();
    const prefix_len = marker + "; exec ".len;
    if (prefix_len + shell.len > out.command.len) return model.provider.defaultArgv();
    @memcpy(out.command[prefix_len..][0..shell.len], shell);
    out.slots[command_index] = out.command[0 .. prefix_len + shell.len];
    return out.slots[0..argv.len];
}

fn contrastRatio(r: f32, g: f32, b: f32, br: f32, bg: f32, bb: f32) f32 {
    const a = linear(r) * 0.2126 + linear(g) * 0.7152 + linear(b) * 0.0722;
    const z = linear(br) * 0.2126 + linear(bg) * 0.7152 + linear(bb) * 0.0722;
    return (@max(a, z) + 0.05) / (@min(a, z) + 0.05);
}

/// Resolve symlinks before atomic replacement so dotfile-manager links survive.
fn destination(allocator: std.mem.Allocator, io: std.Io, path: []const u8) ![]u8 {
    const cwd = std.Io.Dir.cwd();
    return cwd.realPathFileAlloc(io, path, allocator) catch |err| switch (err) {
        error.FileNotFound => missingDestination(allocator, io, path),
        else => return err,
    };
}

fn missingDestination(allocator: std.mem.Allocator, io: std.Io, path: []const u8) ![]u8 {
    const cwd = std.Io.Dir.cwd();
    var link_buffer: [std.fs.max_path_bytes]u8 = undefined;
    // A dangling dotfile-manager link is not an absent file.
    if (cwd.readLink(io, path, &link_buffer)) |_| return error.DanglingConfigLink else |err| switch (err) {
        error.FileNotFound, error.NotLink => {},
        else => return err,
    }
    const parent = std.fs.path.dirname(path) orelse ".";
    try cwd.createDirPath(io, parent);
    const resolved = try cwd.realPathFileAlloc(io, parent, allocator);
    defer allocator.free(resolved);
    return std.fs.path.join(allocator, &.{ resolved, std.fs.path.basename(path) });
}

const Source = struct { bytes: []const u8, permissions: std.Io.File.Permissions };

fn readDestination(io: std.Io, path: []const u8, buffer: []u8) !Source {
    const file = std.Io.Dir.cwd().openFile(io, path, .{ .allow_directory = false }) catch |err| switch (err) {
        error.FileNotFound => return .{ .bytes = "", .permissions = .default_file },
        else => return err,
    };
    defer file.close(io);
    const stat = try file.stat(io);
    const length = try file.readPositionalAll(io, buffer, 0);
    if (length > config.max_config_bytes) return error.ConfigTooLarge;
    try validateDestination(buffer[0..length]);
    return .{ .bytes = buffer[0..length], .permissions = stat.permissions };
}

fn persist(model: *const Model, before: Values, expected_hash: ?u64) !void {
    const io = model.provider.io;
    const allocator = std.heap.page_allocator;
    const path = try destination(allocator, io, model.config_file.path());
    defer allocator.free(path);
    var source: [config.max_config_bytes + 1]u8 = undefined;
    var rewritten: [config.max_config_bytes]u8 = undefined;
    var scratch: [config.max_config_bytes]u8 = undefined;
    const original = try readDestination(io, path, &source);
    if (expected_hash != hashSource(original.bytes)) return error.ExternalConfigChange;
    try writableDestination(io, path);
    const bytes = try rewrite(model, before, original.bytes, &rewritten, &scratch);
    try writeDestination(io, path, bytes, original.permissions);
}

fn writableDestination(io: std.Io, path: []const u8) !void {
    const file = std.Io.Dir.cwd().openFile(io, path, .{ .mode = .read_write, .allow_directory = false }) catch |err| switch (err) {
        error.FileNotFound => return,
        else => return err,
    };
    file.close(io);
}

fn writeDestination(io: std.Io, path: []const u8, bytes: []const u8, permissions: std.Io.File.Permissions) !void {
    var atomic = try std.Io.Dir.cwd().createFileAtomic(io, path, .{ .replace = true, .permissions = permissions });
    defer atomic.deinit(io);
    try atomic.file.writePositionalAll(io, bytes, 0);
    try atomic.file.sync(io);
    try atomic.replace(io);
}

fn validateDestination(source: []const u8) !void {
    const parsed = config.parse(source);
    if (parsed.has_errors) return error.MalformedConfig;
}

fn rewrite(model: *const Model, before: Values, source: []const u8, out: []u8, scratch: []u8) ![]const u8 {
    @memcpy(out[0..source.len], source);
    var result: []const u8 = out[0..source.len];
    inline for (fields, 0..) |pair, id| {
        if (fieldChanged(before, model, id)) {
            if (id == 7) result = try removeKey(result, "command", out, scratch);
            var buffer: [config.max_shell_bytes]u8 = undefined;
            const value = fieldValue(model, id, &buffer);
            result = try editKey(result, pair[0], value, out, scratch);
        }
    }
    return rewriteBindings(&before.config.keybindings, &model.config.keybindings, result, out, scratch);
}

fn bindingEqual(a: config.keybindings_module.Override, b: config.keybindings_module.Override) bool {
    const first = a.chord orelse return b.chord == null;
    const second = b.chord orelse return false;
    return first.eql(second);
}

fn rewriteBindings(before: *const config.keybindings_module.Overrides, current: *const config.keybindings_module.Overrides, source: []const u8, out: []u8, scratch: []u8) ![]const u8 {
    var result = source;
    for (before.entries[0..before.count]) |entry| {
        if (current.indexOf(entry.id.slice()) != null) continue;
        var key: [config.keybindings_module.max_id_bytes + 8]u8 = undefined;
        result = try removeKey(result, try std.fmt.bufPrint(&key, "keybind.{s}", .{entry.id.slice()}), out, scratch);
    }
    for (current.entries[0..current.count]) |entry| {
        if (before.indexOf(entry.id.slice())) |index| {
            if (bindingEqual(before.entries[index], entry)) continue;
        }
        result = try writeBinding(entry, result, out, scratch);
    }
    return result;
}

fn writeBinding(entry: config.keybindings_module.Override, source: []const u8, out: []u8, scratch: []u8) ![]const u8 {
    var key: [config.keybindings_module.max_id_bytes + 8]u8 = undefined;
    const name = try std.fmt.bufPrint(&key, "keybind.{s}", .{entry.id.slice()});
    const chord = if (entry.chord) |value| value.format() else try config.keybindings_module.Text(config.keybindings_module.max_chord_bytes).init("none");
    return editKey(source, name, chord.slice(), out, scratch);
}

fn fieldChanged(before: Values, model: *const Model, comptime id: usize) bool {
    if (id == 1) return before.fontChanged(model);
    if (id == 2) return before.themeChanged(model);
    if (id == 9) return before.placement != model.tab_placement;
    if (@typeInfo(@TypeOf(@field(before.config, fields[id][1]))) == .@"struct") {
        return !std.mem.eql(u8, @field(before.config, fields[id][1]).slice(), @field(model.config, fields[id][1]).slice());
    }
    return !std.meta.eql(@field(before.config, fields[id][1]), @field(model.config, fields[id][1]));
}

fn fieldValue(model: *const Model, comptime id: usize, buffer: []u8) []const u8 {
    if (id == 1) return std.fmt.bufPrint(buffer, "{d}", .{model.fontSize()}) catch unreachable;
    if (id == 2 and model.config.follow_system_theme) return "auto";
    if (id == 9) return @tagName(model.tab_placement);
    return configValue(&model.config, id, buffer);
}

fn configValue(current: *const config.Config, comptime id: usize, buffer: []u8) []const u8 {
    const value = @field(current, fields[id][1]);
    return switch (@typeInfo(@TypeOf(value))) {
        .@"struct" => @field(current, fields[id][1]).slice(),
        .@"enum" => @tagName(value),
        .bool => if (value) "true" else "false",
        else => std.fmt.bufPrint(buffer, "{d}", .{value}) catch unreachable,
    };
}

fn editedConfig(current: config.Config, id: u8, value: []const u8) !config.Config {
    if (std.mem.indexOfAny(u8, value, "\r\n\x00") != null) return error.InvalidValue;
    inline for (fields, 0..) |pair, index| {
        if (id == index) {
            var buffer: [config.max_shell_bytes + 64]u8 = undefined;
            const line = try std.fmt.bufPrint(&buffer, "{s} = {s}", .{ pair[0], value });
            const parsed = config.parse(line);
            if (parsed.diagnostic_count != 0) return error.InvalidValue;
            var next = current;
            @field(next, pair[1]) = @field(parsed, pair[1]);
            if (id == 2) next.follow_system_theme = parsed.follow_system_theme;
            return next;
        }
    }
    return error.InvalidSetting;
}

fn hashSource(bytes: []const u8) u64 {
    return std.hash.Wyhash.hash(0, bytes);
}

fn sourceHash(model: *const Model) !?u64 {
    if (!model.config_file.enabled()) return null;
    var buffer: [config.max_config_bytes + 1]u8 = undefined;
    const original = try readDestination(model.provider.io, model.config_file.path(), &buffer);
    return hashSource(original.bytes);
}

/// Reload only client preferences; an external edit cannot retarget live work.
fn reloadConfig(model: *Model, registry: ?*const config.keybindings_module.Registry) !void {
    if (!model.config_file.enabled()) return error.NoDestination;
    var buffer: [config.max_config_bytes + 1]u8 = undefined;
    const original = try readDestination(model.provider.io, model.config_file.path(), &buffer);
    const parsed = config.parse(original.bytes);
    try validateBindings(&parsed, registry);
    inline for (fields) |pair| @field(model.config, pair[1]) = @field(parsed, pair[1]);
    model.config.follow_system_theme = parsed.follow_system_theme;
    model.config.keybindings = parsed.keybindings;
    model.config.background = parsed.background;
    model.config.foreground = parsed.foreground;
    model.config.selection_background = parsed.selection_background;
    model.config.selection_foreground = parsed.selection_foreground;
    model.config.cursor_color = parsed.cursor_color;
    model.config.window_padding = parsed.window_padding;
    model.config.hide_chrome_when_single = parsed.hide_chrome_when_single;
    model.config.palette = parsed.palette;
    model.config.diagnostics = parsed.diagnostics;
    model.config.diagnostic_count = parsed.diagnostic_count;
    model.config.has_errors = parsed.has_errors;
    model.font_size_offset = 0;
    model.tab_placement = if (parsed.tab_placement == .side) .side else .top;
    applyLocalDefaults(model);
}

fn validateBindings(candidate: *const config.Config, registry: ?*const config.keybindings_module.Registry) !void {
    if (candidate.keybindings.count == 0) return;
    const commands = registry orelse return error.BindingRegistryUnavailable;
    _ = try commands.resolve(&candidate.keybindings);
}

test "external config edits during an appearance preview are not overwritten" {
    const Engine = @import("ts_engine.zig").Engine;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.testing.io;
    try tmp.dir.writeFile(io, .{ .sub_path = "config", .data = "font-size = 13\n" });
    const path = try tmp.dir.realPathFileAlloc(io, "config", std.testing.allocator);
    defer std.testing.allocator.free(path);
    engine.model.config_file.setPath(path);
    var state: State = .{};
    state.apply(engine.model, &.{ 1, 0, 0 });
    state.apply(engine.model, &.{ 1, 2, 0 });
    const external = "# external editor\nfont-size = 19\nfuture-key = keep\n";
    try tmp.dir.writeFile(io, .{ .sub_path = "config", .data = external });
    state.apply(engine.model, &.{ 1, 7, 0 });
    // Assert behavior, not the new outcome enum: this regression also runs
    // against the prior writer, which incorrectly reports a successful save.
    try std.testing.expect(state.initial != null);
    var buffer: [256]u8 = undefined;
    const source = try readDestination(io, path, &buffer);
    try std.testing.expectEqualStrings(external, source.bytes);
    state.apply(engine.model, &.{ 1, 6, 0 });
    try std.testing.expectEqual(@as(f32, 13), engine.model.fontSize());
}

test "settings preview reset cancel and save preserve actual defaults and comments" {
    const Engine = @import("ts_engine.zig").Engine;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.testing.io;
    const original = "# personal config\nbackground = #010203\nfuture-key = retained\n";
    try tmp.dir.writeFile(io, .{ .sub_path = "config", .data = original });
    const path = try tmp.dir.realPathFileAlloc(io, "config", std.testing.allocator);
    defer std.testing.allocator.free(path);
    const model = engine.model;
    model.config_file.setPath(path);
    model.config.background = .{ .r = 1, .g = 2, .b = 3 };
    const topology = model.topologyFingerprint();
    var state: State = .{ .system_scheme = .light };
    state.apply(model, &.{ 1, 0, 0 });
    state.apply(model, "\x02\x08\x00Geist Mono");
    state.apply(model, "\x02\x08\x02auto");
    state.apply(model, "\x02\x08\x034.5");
    state.apply(model, "\x02\x08\x05false");
    state.apply(model, "\x02\x08\x085false"); // invalid boolean leaves last good
    try std.testing.expectEqual(.invalid, state.outcome);
    state.apply(model, "\x02\x08\x08false");
    state.apply(model, "\x02\x08\x0anvim --wait");
    try std.testing.expect(model.config.follow_system_theme);
    try std.testing.expectEqualStrings(themes.forScheme(.light), model.config.theme.slice());
    try std.testing.expectEqualStrings("Geist Mono", model.config.font_family.slice());
    try std.testing.expectEqual(@as(f32, 4.5), model.config.minimum_contrast);
    try std.testing.expect(!model.config.cursor_style_blink);
    try std.testing.expect(!model.config.inherit_working_directory);
    try std.testing.expectEqual(topology, model.topologyFingerprint());
    try std.testing.expectEqual(@as(u8, 1), model.config.resolvedBackground().?.r);
    state.apply(model, &.{ 2, 9, 3 });
    try std.testing.expectEqual(config.default_minimum_contrast, model.config.minimum_contrast);
    state.apply(model, &.{ 1, 7, 0 });
    try std.testing.expectEqual(.saved, state.outcome);
    var buffer: [1024]u8 = undefined;
    const source = try readDestination(io, path, &buffer);
    try std.testing.expect(std.mem.startsWith(u8, source.bytes, original));
    try std.testing.expect(std.mem.indexOf(u8, source.bytes, "theme = auto\n") != null);
    try std.testing.expect(std.mem.indexOf(u8, source.bytes, "minimum-contrast") == null);
    try std.testing.expectEqualStrings("nvim --wait", config.parse(source.bytes).editorCommand());
    state.apply(model, &.{ 1, 0, 0 });
    state.apply(model, &.{ 2, 9, 0 });
    state.apply(model, &.{ 2, 9, 10 });
    state.apply(model, &.{ 1, 6, 0 });
    try std.testing.expectEqualStrings("Geist Mono", model.config.font_family.slice());
    try std.testing.expectEqualStrings("nvim --wait", model.config.editorCommand());
}

test "malformed reload keeps last good values and valid reload never retargets work" {
    const Engine = @import("ts_engine.zig").Engine;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.testing.io;
    try tmp.dir.writeFile(io, .{ .sub_path = "config", .data = "font-size = 20\nbroken line\n" });
    const path = try tmp.dir.realPathFileAlloc(io, "config", std.testing.allocator);
    defer std.testing.allocator.free(path);
    engine.model.config_file.setPath(path);
    engine.model.config.font_size = 17;
    _ = engine.model.config.setPhuxSocket("/safe/socket", .environment);
    var state: State = .{};
    state.apply(engine.model, &.{ 2, 10, 0 });
    try std.testing.expectEqual(.malformed, state.outcome);
    try std.testing.expectEqual(@as(f32, 17), engine.model.config.font_size);
    try tmp.dir.writeFile(io, .{ .sub_path = "config", .data = "font-size = 20\nphux-socket = /different/socket\n" });
    state.apply(engine.model, &.{ 2, 10, 0 });
    try std.testing.expectEqual(.saved, state.outcome);
    try std.testing.expectEqual(@as(f32, 20), engine.model.config.font_size);
    try std.testing.expectEqualStrings("/safe/socket", engine.model.config.phux_socket.slice());
}

test "binding persistence changes only edited overrides and removes reset keys" {
    const bindings = config.keybindings_module;
    var before: bindings.Overrides = .{};
    try before.set("view.settings", "Cmd+Alt+s");
    try before.set("view.clear", "none");
    var current = before;
    current.reset("view.settings");
    try current.set("view.clear", "Cmd+Alt+k");
    const original = "# keyboard\nkeybind.view.settings=Cmd+Alt+s\nkeybind.view.clear=none\nfuture = retained\n";
    var out: [1024]u8 = undefined;
    var scratch: [1024]u8 = undefined;
    const result = try rewriteBindings(&before, &current, original, &out, &scratch);
    try std.testing.expectEqualStrings("# keyboard\nkeybind.view.clear = Cmd+Alt+k\nfuture = retained\n", result);
    try std.testing.expectEqual(@as(usize, 1), config.parse(result).keybindings.count);
}

test "shell preview affects new scratch argv with quoted cwd and cancel restores defaults" {
    const Engine = @import("ts_engine.zig").Engine;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    var state: State = .{};
    state.apply(model, &.{ 1, 0, 0 });
    state.apply(model, "\x02\x08\x07/bin/sh -i");
    state.apply(model, "\x02\x08\x061048576");
    const argv = model.provider.defaultArgv();
    try std.testing.expectEqualStrings("exec /bin/sh -i", argv[argv.len - 1]);
    const inherited = scratchArgvIn(model, "/work/o'brien; exec nope", 0);
    try std.testing.expectEqualStrings("cd '/work/o'\\''brien; exec nope' 2>/dev/null || cd \"$HOME\"; exec /bin/sh -i", inherited[inherited.len - 1]);
    const small = try model.provider.createTerminal();
    const small_size = small.session.term.screens.active.pages.maxSize();
    state.apply(model, &.{ 1, 6, 0 });
    const large = try model.provider.createTerminal();
    try std.testing.expect(small_size < large.session.term.screens.active.pages.maxSize());
    try std.testing.expect(!model.provider.shell_command.isSet());
}

test "binding conflict reload retains last good client settings" {
    const Engine = @import("ts_engine.zig").Engine;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var registry: config.keybindings_module.Registry = .{};
    try registry.add(.{ .id = "view.settings", .default = try config.keybindings_module.Chord.parse("Cmd+,") });
    try registry.add(.{ .id = "view.clear", .default = try config.keybindings_module.Chord.parse("Cmd+k") });
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.testing.io;
    try tmp.dir.writeFile(io, .{ .sub_path = "config", .data = "font-size = 20\nkeybind.view.settings = Cmd+k\n" });
    const path = try tmp.dir.realPathFileAlloc(io, "config", std.testing.allocator);
    defer std.testing.allocator.free(path);
    engine.model.config_file.setPath(path);
    var state: State = .{ .binding_registry = &registry };
    state.apply(engine.model, &.{ 2, 10, 0 });
    try std.testing.expectEqual(.refused, state.outcome);
    try std.testing.expectEqual(config.default_font_size, engine.model.fontSize());
    try std.testing.expectEqual(@as(usize, 0), engine.model.config.keybindings.count);
}

// Failure injection exercises the Settings/installer transaction seam. Actual
// AppKit registration and failed-platform-rollback evidence live with the host.
const TestBindingInstaller = struct {
    fail: bool = true,
    calls: usize = 0,
    attempted: config.keybindings_module.Overrides = .{},
    applied: config.keybindings_module.Overrides = .{},

    pub fn sync(self: *TestBindingInstaller, overrides: *const config.keybindings_module.Overrides) !void {
        self.calls += 1;
        self.attempted = overrides.*;
        if (self.fail) return error.BindingInstallationFailed;
        self.applied = overrides.*;
    }
};

fn testBindingRegistry() !config.keybindings_module.Registry {
    var registry: config.keybindings_module.Registry = .{};
    try registry.add(.{ .id = "view.settings", .default = try config.keybindings_module.Chord.parse("Cmd+,") });
    return registry;
}

fn expectTransitionRestored(before: TransitionSnapshot, state: *const State, model: *const Model) !void {
    const testing = std.testing;
    try testing.expectEqualDeep(before.config, model.config);
    try testing.expectEqual(before.font_offset, model.font_size_offset);
    try testing.expectEqual(before.placement, model.tab_placement);
    try testing.expectEqual(before.committed_placement, model.appearance_committed_placement);
    try testing.expectEqual(before.write_refused, model.config_write_refused);
    try testing.expectEqualDeep(before.state.initial, state.initial);
    try testing.expectEqual(before.state.source_hash, state.source_hash);
    try testing.expectEqual(before.state.read_error, state.read_error);
    try testing.expectEqual(before.state.system_scheme, state.system_scheme);
    try testing.expectEqual(before.state.binding_registry, state.binding_registry);
    try testing.expectEqual(.refused, state.outcome);
    try testing.expectEqual(before.config.scrollback_bytes, model.provider.max_scrollback_bytes);
    const pane = model.provider.terminalConst(model.focusedTerminalRef().?).?;
    try testing.expectEqualStrings(@tagName(before.config.cursor_style), @tagName(pane.session.term.cursor.default_style));
    try testing.expectEqual(@as(?bool, before.config.cursor_style_blink), pane.session.term.cursor.default_blink);
    var buffer: [config.max_shell_bytes + 5]u8 = undefined;
    const expected = try std.fmt.bufPrint(&buffer, "exec {s}", .{before.config.shell.slice()});
    const argv = model.provider.defaultArgv();
    try testing.expectEqualStrings(expected, argv[argv.len - 1]);
}

test "failed binding installation on Cancel restores preview and opening transaction" {
    const Engine = @import("ts_engine.zig").Engine;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    var registry = try testBindingRegistry();
    var state: State = .{ .binding_registry = &registry };
    state.apply(model, &.{ 1, 0, 0 });
    state.apply(model, "\x02\x08\x07/bin/sh -i");
    state.apply(model, "\x02\x08\x061048576");
    state.apply(model, "\x02\x08\x05false");
    state.apply(model, "\x02\x08\x04bar");
    state.apply(model, &.{ 1, 5, 1 });
    model.font_size_offset = 2;
    model.config_write_refused = true;
    try model.config.keybindings.set("view.settings", "Cmd+Alt+s");
    const before = TransitionSnapshot.capture(state, model);
    var installer: TestBindingInstaller = .{ .applied = model.config.keybindings };

    try std.testing.expectError(error.BindingInstallationFailed, state.applyWithBindings(model, &.{ 1, 6, 0 }, &installer));
    try std.testing.expectEqual(@as(usize, 0), installer.attempted.count); // the opening bindings were attempted
    try expectTransitionRestored(before, &state, model);
    try std.testing.expectEqualDeep(before.config.keybindings, installer.applied);
    try std.testing.expect(state.hasPendingChanges(model));

    installer.fail = false;
    try state.applyWithBindings(model, &.{ 1, 6, 0 }, &installer);
    try std.testing.expectEqual(.canceled, state.outcome);
    try std.testing.expect(state.initial == null);
    try std.testing.expectEqual(@as(usize, 0), installer.applied.count);
    try std.testing.expect(!model.provider.shell_command.isSet());
}

test "failed binding installation on Reload restores full last-good config and local defaults" {
    const Engine = @import("ts_engine.zig").Engine;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.testing.io;
    const incoming = "font-size = 20\nshell = /bin/zsh -i\ncursor-style = underline\ntab-placement = top\nkeybind.view.settings = Cmd+Alt+s\n";
    try tmp.dir.writeFile(io, .{ .sub_path = "config", .data = incoming });
    const path = try tmp.dir.realPathFileAlloc(io, "config", std.testing.allocator);
    defer std.testing.allocator.free(path);
    const model = engine.model;
    model.config_file.setPath(path);
    model.config = config.parse("font-size = 16\nshell = /bin/sh -i\ncursor-style = bar\ncursor-style-blink = false\nscrollback-limit = 1048576\n");
    _ = model.config.setPhuxSocket("/retained/socket", .environment);
    model.font_size_offset = 3;
    model.tab_placement = .side;
    model.appearance_committed_placement = .top;
    model.config_write_refused = true;
    applyLocalDefaults(model);
    var registry = try testBindingRegistry();
    var state: State = .{ .binding_registry = &registry, .system_scheme = .light };
    const before = TransitionSnapshot.capture(state, model);
    var installer: TestBindingInstaller = .{};

    try std.testing.expectError(error.BindingInstallationFailed, state.applyWithBindings(model, &.{ 2, 10, 0 }, &installer));
    try std.testing.expectEqual(@as(usize, 1), installer.attempted.count);
    try expectTransitionRestored(before, &state, model);
    try std.testing.expectEqualDeep(before.config.keybindings, installer.applied);
    var buffer: [1024]u8 = undefined;
    try std.testing.expectEqualStrings(incoming, (try readDestination(io, path, &buffer)).bytes);

    installer.fail = false;
    try state.applyWithBindings(model, &.{ 2, 10, 0 }, &installer);
    try std.testing.expectEqual(.saved, state.outcome);
    try std.testing.expectEqual(@as(f32, 20), model.fontSize());
    try std.testing.expectEqualStrings("/retained/socket", model.config.phux_socket.slice());
    try std.testing.expectEqualDeep(model.config.keybindings, installer.applied);
    const pane = model.provider.terminal(model.focusedTerminalRef().?).?;
    try std.testing.expectEqual(.underline, pane.session.term.cursor.default_style);
}

test "failed binding installation on Save leaves disk untouched and successful retry persists" {
    const Engine = @import("ts_engine.zig").Engine;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.testing.io;
    const original = "# preserve this file\nfont-size = 13\nfuture-key = retained\n";
    try tmp.dir.writeFile(io, .{ .sub_path = "config", .data = original });
    const path = try tmp.dir.realPathFileAlloc(io, "config", std.testing.allocator);
    defer std.testing.allocator.free(path);
    const model = engine.model;
    model.config_file.setPath(path);
    var registry = try testBindingRegistry();
    var state: State = .{ .binding_registry = &registry };
    state.apply(model, &.{ 1, 0, 0 });
    state.apply(model, "\x02\x08\x07/bin/sh -i");
    state.apply(model, &.{ 1, 2, 0 });
    try model.config.keybindings.set("view.settings", "Cmd+Alt+s");
    model.config_write_refused = true;
    const before = TransitionSnapshot.capture(state, model);
    var installer: TestBindingInstaller = .{ .applied = model.config.keybindings };

    try std.testing.expectError(error.BindingInstallationFailed, state.applyWithBindings(model, &.{ 1, 7, 0 }, &installer));
    try std.testing.expectEqual(@as(usize, 1), installer.calls);
    try std.testing.expectEqualDeep(before.config.keybindings, installer.attempted);
    try expectTransitionRestored(before, &state, model);
    var buffer: [1024]u8 = undefined;
    try std.testing.expectEqualStrings(original, (try readDestination(io, path, &buffer)).bytes);

    installer.fail = false;
    try state.applyWithBindings(model, &.{ 1, 7, 0 }, &installer);
    try std.testing.expectEqual(.saved, state.outcome);
    try std.testing.expect(state.initial == null);
    const written = try readDestination(io, path, &buffer);
    const loaded = config.parse(written.bytes);
    try std.testing.expectEqual(@as(f32, 14), loaded.font_size);
    try std.testing.expectEqualDeep(model.config.keybindings, loaded.keybindings);
    try std.testing.expect(std.mem.indexOf(u8, written.bytes, "future-key = retained\n") != null);
}

fn editKey(source: []const u8, key: []const u8, value: []const u8, out: []u8, scratch: []u8) ![]const u8 {
    const next = try config.setKey(source, key, value, scratch);
    @memcpy(out[0..next.len], next);
    return out[0..next.len];
}

fn removeKey(source: []const u8, key: []const u8, out: []u8, scratch: []u8) ![]const u8 {
    const next = try config.removeKey(source, key, scratch);
    @memcpy(out[0..next.len], next);
    return out[0..next.len];
}
