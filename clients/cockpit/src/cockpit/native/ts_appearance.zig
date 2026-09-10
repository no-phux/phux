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
pub const max_bytes = 128;
pub const Outcome = enum(u8) { preview, saved, canceled, refused, no_destination, invalid };

const Values = struct {
    theme: config.ThemeName,
    follow_system: bool,
    font_size: f32,
    font_offset: f32,
    cursor: config.CursorStyle,
    placement: TabPlacement,

    fn capture(model: *const Model) Values {
        return .{ .theme = model.config.theme, .follow_system = model.config.follow_system_theme, .font_size = model.config.font_size, .font_offset = model.font_size_offset, .cursor = model.config.cursor_style, .placement = model.tab_placement };
    }

    fn restore(self: Values, model: *Model) void {
        model.config.theme = self.theme;
        model.config.follow_system_theme = self.follow_system;
        model.config.font_size = self.font_size;
        model.font_size_offset = self.font_offset;
        model.config.cursor_style = self.cursor;
        applyLocalCursor(model);
        model.tab_placement = self.placement;
        model.appearance_committed_placement = null;
    }

    fn themeChanged(self: Values, model: *const Model) bool {
        return self.follow_system != model.config.follow_system_theme or
            !std.mem.eql(u8, self.theme.slice(), model.config.theme.slice());
    }

    fn fontChanged(self: Values, model: *const Model) bool {
        return self.font_size != model.config.font_size or self.font_offset != model.font_size_offset;
    }

    fn changed(self: Values, model: *const Model) bool {
        return self.themeChanged(model) or self.fontChanged(model) or
            self.cursor != model.config.cursor_style or self.placement != model.tab_placement;
    }
};

pub const State = struct {
    initial: ?Values = null,
    outcome: Outcome = .preview,

    /// Requests are synchronous on the app loop; the core admits one at a time.
    /// Begin is idempotent, so reopening cannot overwrite the rollback point.
    pub fn apply(self: *State, model: *Model, bytes: []const u8) void {
        if (bytes.len != 3 or bytes[0] != 1) {
            self.outcome = .invalid;
            return;
        }
        if (bytes[1] == 0) {
            if (self.initial == null) {
                self.initial = Values.capture(model);
                model.appearance_committed_placement = model.tab_placement;
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

    fn edit(self: *State, model: *Model, before: Values, action: u8, argument: u8) void {
        switch (action) {
            1 => self.previewTheme(model, argument),
            2 => _ = model.stepFontSize(1),
            3 => _ = model.stepFontSize(-1),
            4 => self.previewCursor(model, argument),
            5 => self.previewPlacement(model, argument),
            6 => {
                before.restore(model);
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
        applyLocalCursor(model);
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
        persist(model, before) catch {
            self.outcome = .refused;
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
        out[0] = 1;
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
        return out[0 .. 10 + font.len + contrast.len];
    }
};

fn linear(value: f32) f32 {
    return if (value <= 0.04045) value / 12.92 else std.math.pow(f32, (value + 0.055) / 1.055, 2.4);
}

fn applyLocalCursor(model: *Model) void {
    var refs: [local.max_terminals]TerminalRef = undefined;
    const count = model.provider.terminalRefs(&refs);
    for (refs[0..count]) |ref| {
        const pane = model.provider.terminal(ref) orelse continue;
        pane.session.term.setDefaultCursorStyle(switch (model.config.cursor_style) {
            .block => .block,
            .bar => .bar,
            .underline => .underline,
        });
    }
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
    const file = std.Io.Dir.cwd().openFile(io, path, .{ .mode = .read_write, .allow_directory = false }) catch |err| switch (err) {
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

fn persist(model: *const Model, before: Values) !void {
    const io = model.provider.io;
    const allocator = std.heap.page_allocator;
    const path = try destination(allocator, io, model.config_file.path());
    defer allocator.free(path);
    var source: [config.max_config_bytes + 1]u8 = undefined;
    var rewritten: [config.max_config_bytes]u8 = undefined;
    var scratch: [config.max_config_bytes]u8 = undefined;
    const original = try readDestination(io, path, &source);
    const bytes = try rewrite(model, before, original.bytes, &rewritten, &scratch);
    try writeDestination(io, path, bytes, original.permissions);
}

fn writeDestination(io: std.Io, path: []const u8, bytes: []const u8, permissions: std.Io.File.Permissions) !void {
    var atomic = try std.Io.Dir.cwd().createFileAtomic(io, path, .{ .replace = true, .permissions = permissions });
    defer atomic.deinit(io);
    try atomic.file.writePositionalAll(io, bytes, 0);
    try atomic.file.sync(io);
    try atomic.replace(io);
}

fn validateDestination(source: []const u8) !void {
    // The parser's UI diagnostics are capped. Parse each physical line so
    // preserved unknown keys cannot hide a malformed value beyond that cap.
    var lines = std.mem.splitScalar(u8, source, '\n');
    while (lines.next()) |line| {
        const parsed = config.parse(line);
        for (parsed.diagnosticSlice()) |diagnostic| {
            switch (diagnostic.kind) {
                .bad_value, .missing_separator => return error.MalformedConfig,
                else => {},
            }
        }
    }
}

fn rewrite(model: *const Model, before: Values, source: []const u8, out: []u8, scratch: []u8) ![]const u8 {
    @memcpy(out[0..source.len], source);
    var result: []const u8 = out[0..source.len];
    if (before.themeChanged(model)) result = try editKey(result, "theme", model.config.theme.slice(), out, scratch);
    if (before.fontChanged(model)) {
        var number: [32]u8 = undefined;
        result = try editKey(result, "font-size", try std.fmt.bufPrint(&number, "{d}", .{model.fontSize()}), out, scratch);
    }
    if (before.cursor != model.config.cursor_style) result = try editKey(result, "cursor-style", @tagName(model.config.cursor_style), out, scratch);
    if (before.placement != model.tab_placement) result = try editKey(result, "tab-placement", @tagName(model.tab_placement), out, scratch);
    return result;
}

fn editKey(source: []const u8, key: []const u8, value: []const u8, out: []u8, scratch: []u8) ![]const u8 {
    const next = try config.setKey(source, key, value, scratch);
    @memcpy(out[0..next.len], next);
    return out[0..next.len];
}
