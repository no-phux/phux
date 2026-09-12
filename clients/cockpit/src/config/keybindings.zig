//! Command bindings, independent of the UI and platform. Only Command chords
//! are app-owned; Control/Option input continues to belong to terminal programs.
const std = @import("std");

// Match the pinned SDK's registration and command identifier bounds.
pub const max_overrides = 64;
pub const max_commands = 192; // 128 menu items plus 64 shortcut-only commands.
pub const max_id_bytes = 128;
pub const max_chord_bytes = 64;
pub const cmd: u8 = 1;
pub const ctrl: u8 = 2;
pub const alt: u8 = 4;
pub const shift: u8 = 8;

pub fn Text(comptime capacity: usize) type {
    return struct {
        bytes: [capacity]u8 = @splat(0),
        len: usize = 0,
        pub fn init(value: []const u8) !@This() {
            if (value.len > capacity) return error.TooLong;
            var result: @This() = .{ .len = value.len };
            @memcpy(result.bytes[0..value.len], value);
            return result;
        }
        pub fn slice(self: *const @This()) []const u8 {
            return self.bytes[0..self.len];
        }
    };
}

pub const Chord = struct {
    key: Text(32) = .{},
    modifiers: u8 = 0,

    pub fn parse(value: []const u8) !Chord {
        if (value.len > max_chord_bytes) return error.TooLong;
        var parts = std.mem.splitScalar(u8, value, '+');
        var result: Chord = .{};
        while (parts.next()) |part| {
            const token = std.mem.trim(u8, part, " \t");
            if (parts.peek() == null) {
                result.key = try normalizedKey(token);
                break;
            }
            const modifier = modifierBit(token) orelse return error.InvalidModifier;
            if (result.modifiers & modifier != 0) return error.DuplicateModifier;
            result.modifiers |= modifier;
        }
        if (result.modifiers & cmd == 0) return error.TerminalOwnedChord;
        if (reserved(result)) return error.ReservedChord;
        return result;
    }

    pub fn eql(self: Chord, other: Chord) bool {
        return self.modifiers == other.modifiers and std.mem.eql(u8, self.key.slice(), other.key.slice());
    }

    pub fn format(self: Chord) Text(max_chord_bytes) {
        var result: Text(max_chord_bytes) = .{};
        const names = [_][]const u8{ "Cmd+", "Ctrl+", "Alt+", "Shift+" };
        for (names, 0..) |name, index| {
            const bit = @as(u8, 1) << @as(u3, @intCast(index));
            if (self.modifiers & bit != 0) append(&result, name);
        }
        append(&result, self.key.slice());
        return result;
    }
};

fn append(target: anytype, value: []const u8) void {
    @memcpy(target.bytes[target.len..][0..value.len], value);
    target.len += value.len;
}

fn modifierBit(value: []const u8) ?u8 {
    const names = .{ "cmd", "command", "primary", "super", "ctrl", "control", "alt", "option", "shift" };
    const bits = [_]u8{ cmd, cmd, cmd, cmd, ctrl, ctrl, alt, alt, shift };
    inline for (names, bits) |name, bit| {
        if (std.ascii.eqlIgnoreCase(value, name)) return bit;
    }
    return null;
}

const special_keys = [_][]const u8{
    "escape",  "enter",     "tab",    "space", "backspace", "arrowleft", "arrowright",
    "arrowup", "arrowdown", "delete", "home",  "end",       "pageup",    "pagedown",
    "insert",  "f1",        "f2",     "f3",    "f4",        "f5",        "f6",
    "f7",      "f8",        "f9",     "f10",   "f11",       "f12",
};

pub fn normalizedKey(value: []const u8) !Text(32) {
    if (value.len == 1) {
        const ch = std.ascii.toLower(value[0]);
        if (std.ascii.isAlphanumeric(ch)) return Text(32).init(&.{ch});
        if (std.mem.indexOfScalar(u8, "=-,./;'[]\\`", ch) != null) return Text(32).init(&.{ch});
    }
    for (special_keys) |key| {
        if (std.ascii.eqlIgnoreCase(value, key)) return Text(32).init(key);
    }
    return error.InvalidKey;
}

fn reserved(chord: Chord) bool {
    // AppKit's application menu and macOS own these outside app.zon.
    const values = .{ "q", "h", "tab", "space" };
    inline for (values) |key| {
        if (std.mem.eql(u8, chord.key.slice(), key)) return true;
    }
    return false;
}

pub const Override = struct {
    id: Text(max_id_bytes) = .{},
    chord: ?Chord = null, // Explicit `none`; absence from Overrides means default.
};

pub const Overrides = struct {
    entries: [max_overrides]Override = @splat(.{}),
    count: usize = 0,

    pub fn indexOf(self: *const Overrides, id: []const u8) ?usize {
        for (self.entries[0..self.count], 0..) |*entry, index| {
            if (std.mem.eql(u8, entry.id.slice(), id)) return index;
        }
        return null;
    }

    /// Parsing is deliberately order independent: conflict checks belong to
    /// Registry.resolve after every assignment, so swapping two chords is valid.
    pub fn set(self: *Overrides, id: []const u8, value: []const u8) !void {
        try validateId(id);
        if (std.ascii.eqlIgnoreCase(value, "default")) {
            self.reset(id);
            return;
        }
        const chord = if (std.ascii.eqlIgnoreCase(value, "none")) null else try Chord.parse(value);
        const index = self.indexOf(id) orelse self.count;
        if (index == max_overrides) return error.TooManyOverrides;
        self.entries[index] = .{ .id = try Text(max_id_bytes).init(id), .chord = chord };
        if (index == self.count) self.count += 1;
    }

    pub fn reset(self: *Overrides, id: []const u8) void {
        const index = self.indexOf(id) orelse return;
        self.count -= 1;
        std.mem.copyForwards(Override, self.entries[index..self.count], self.entries[index + 1 .. self.count + 1]);
        self.entries[self.count] = .{};
    }

    pub fn resetAll(self: *Overrides) void {
        self.* = .{};
    }
};

fn validateId(id: []const u8) !void {
    if (id.len == 0 or id.len > max_id_bytes) return error.InvalidCommand;
    for (id) |ch| {
        if (std.ascii.isAlphanumeric(ch)) continue;
        if (std.mem.indexOfScalar(u8, "._-", ch) == null) return error.InvalidCommand;
    }
}

pub const Command = struct {
    id: []const u8 = "",
    label: []const u8 = "",
    default: ?Chord = null,
};

pub const Conflict = struct { first: usize, second: usize };

pub const Resolved = struct {
    chords: [max_commands]?Chord = @splat(null),
    count: usize = 0,

    pub fn conflict(self: *const Resolved) ?Conflict {
        for (self.chords[0..self.count], 0..) |maybe, first| {
            const chord = maybe orelse continue;
            for (self.chords[first + 1 .. self.count], first + 1..) |other, second| {
                if (other) |bound| {
                    if (chord.eql(bound)) return .{ .first = first, .second = second };
                }
            }
        }
        return null;
    }

    /// Exact modifiers win, matching AppKit's two-pass punctuation rule:
    /// Cmd+[ and Cmd+Shift+[ are distinct; the latter wins for shifted input.
    pub fn match(self: *const Resolved, key: []const u8, modifiers: u8) ?usize {
        const normalized = normalizedEventKey(key) catch return null;
        const event: Chord = .{ .key = normalized, .modifiers = modifiers };
        if (self.exact(event)) |index| return index;
        if (modifiers & shift == 0) return null;
        if (!implicitShift(normalized.slice())) return null;
        return self.exact(.{ .key = normalized, .modifiers = modifiers & ~shift });
    }

    fn exact(self: *const Resolved, event: Chord) ?usize {
        for (self.chords[0..self.count], 0..) |maybe, index| {
            const chord = maybe orelse continue;
            if (chord.eql(event)) return index;
        }
        return null;
    }
};

fn normalizedEventKey(key: []const u8) !Text(32) {
    if (key.len != 1) return normalizedKey(key);
    // AppKit normalizes shifted punctuation before shortcut matching.
    const shifted = "!@#$%^&*()+_<>?:\"{}|~";
    const plain = "1234567890=-,./;'[]\\`";
    if (std.mem.indexOfScalar(u8, shifted, key[0])) |index| return normalizedKey(plain[index .. index + 1]);
    return normalizedKey(key);
}

fn implicitShift(key: []const u8) bool {
    if (key.len != 1) return false;
    return std.mem.indexOfScalar(u8, "0123456789=-,./;'[]\\`", key[0]) != null;
}

pub const Registry = struct {
    commands: [max_commands]Command = @splat(.{}),
    count: usize = 0,

    pub fn indexOf(self: *const Registry, id: []const u8) ?usize {
        for (self.commands[0..self.count], 0..) |command, index| {
            if (std.mem.eql(u8, command.id, id)) return index;
        }
        return null;
    }

    /// Command and label slices borrow the process-lifetime shipping manifest.
    pub fn add(self: *Registry, command: Command) !void {
        try validateId(command.id);
        if (self.indexOf(command.id)) |index| {
            try self.merge(index, command);
            return;
        }
        if (self.count == max_commands) return error.TooManyCommands;
        self.commands[self.count] = command;
        self.count += 1;
    }

    fn merge(self: *Registry, index: usize, command: Command) !void {
        const existing = &self.commands[index];
        if (command.default) |chord| {
            if (existing.default) |old| {
                if (!old.eql(chord)) return error.InconsistentDefault;
            }
            existing.default = chord;
        }
        if (command.label.len > 0) existing.label = command.label;
    }

    pub fn resolve(self: *const Registry, overrides: *const Overrides) !Resolved {
        var result = self.defaults();
        for (overrides.entries[0..overrides.count]) |entry| {
            const index = self.indexOf(entry.id.slice()) orelse return error.UnknownCommand;
            result.chords[index] = entry.chord;
        }
        if (result.conflict() != null) return error.ConflictingChord;
        return result;
    }

    pub fn defaults(self: *const Registry) Resolved {
        var result: Resolved = .{ .count = self.count };
        for (self.commands[0..self.count], 0..) |command, index| result.chords[index] = command.default;
        return result;
    }

    /// Edits are transactions: conflicts (including reset collisions) leave
    /// the opening candidate untouched. `none` and reset are different actions.
    pub fn edit(self: *const Registry, overrides: *Overrides, action: u8, index: usize, value: []const u8) !void {
        var candidate = overrides.*;
        if (action == 3) {
            candidate.resetAll();
        } else {
            if (index >= self.count) return error.UnknownCommand;
            const id = self.commands[index].id;
            switch (action) {
                1 => try candidate.set(id, value),
                2 => candidate.reset(id),
                else => return error.InvalidAction,
            }
        }
        _ = try self.resolve(&candidate);
        overrides.* = candidate;
    }
};

test "chords normalize aliases, case and modifier order; reject duplicates" {
    const first = try Chord.parse("SHIFT + primary + K");
    const second = try Chord.parse("cmd+shift+k");
    try std.testing.expect(first.eql(second));
    try std.testing.expectEqualStrings("Cmd+Shift+k", first.format().slice());
    try std.testing.expectError(error.DuplicateModifier, Chord.parse("cmd+primary+k"));
    try std.testing.expectError(error.InvalidModifier, Chord.parse("cmd+hyper+k"));
    try std.testing.expectError(error.InvalidKey, Chord.parse("cmd+"));
}

test "terminal and system owned input cannot be captured by a remap" {
    for ([_][]const u8{ "ctrl+c", "option+arrowleft", "shift+a", "f1" }) |text| {
        try std.testing.expectError(error.TerminalOwnedChord, Chord.parse(text));
    }
    try std.testing.expectError(error.ReservedChord, Chord.parse("cmd+q"));
    try std.testing.expectError(error.ReservedChord, Chord.parse("cmd+alt+h"));
}

fn testRegistry() !Registry {
    var registry: Registry = .{};
    try registry.add(.{ .id = "terminal.new", .label = "New Tab", .default = try Chord.parse("cmd+t") });
    try registry.add(.{ .id = "window.new", .label = "New Window", .default = try Chord.parse("cmd+n") });
    try registry.add(.{ .id = "session.rename", .label = "Rename Session" });
    return registry;
}

test "duplicate menu and shortcut declarations deduplicate, drift fails" {
    var registry = try testRegistry();
    try registry.add(.{ .id = "terminal.new", .default = try Chord.parse("primary+t") });
    try std.testing.expectEqual(@as(usize, 3), registry.count);
    try std.testing.expectError(error.InconsistentDefault, registry.add(.{ .id = "terminal.new", .default = try Chord.parse("cmd+r") }));
}

test "conflicting edits are atomic and unknown commands are rejected" {
    const registry = try testRegistry();
    var overrides: Overrides = .{};
    try std.testing.expectError(error.ConflictingChord, registry.edit(&overrides, 1, 0, "primary+n"));
    try std.testing.expectEqual(@as(usize, 0), overrides.count);
    try overrides.set("typo.command", "cmd+x");
    try std.testing.expectError(error.UnknownCommand, registry.resolve(&overrides));
}

test "swaps parse independently of order and reset collisions preserve overrides" {
    const registry = try testRegistry();
    var overrides: Overrides = .{};
    try overrides.set("terminal.new", "cmd+n");
    try overrides.set("window.new", "cmd+t");
    const resolved = try registry.resolve(&overrides);
    try std.testing.expectEqual(@as(?usize, 0), resolved.match("n", cmd));
    try std.testing.expectError(error.ConflictingChord, registry.edit(&overrides, 2, 0, ""));
    try std.testing.expectEqual(@as(usize, 2), overrides.count);
    try registry.edit(&overrides, 3, 0, "");
    try std.testing.expectEqual(@as(usize, 0), overrides.count);
    try std.testing.expectEqual(@as(?usize, 0), (try registry.resolve(&overrides)).match("t", cmd));
}

test "none disables while reset restores the shipping default" {
    const registry = try testRegistry();
    var overrides: Overrides = .{};
    try registry.edit(&overrides, 1, 0, "none");
    try std.testing.expect((try registry.resolve(&overrides)).match("t", cmd) == null);
    try registry.edit(&overrides, 2, 0, "");
    try std.testing.expectEqual(@as(?usize, 0), (try registry.resolve(&overrides)).match("t", cmd));
}

test "all modifiers match exactly and shifted punctuation uses AppKit precedence" {
    var registry: Registry = .{};
    try registry.add(.{ .id = "pane.previous", .default = try Chord.parse("cmd+[") });
    try registry.add(.{ .id = "tab.previous", .default = try Chord.parse("cmd+shift+[") });
    try registry.add(.{ .id = "view.font-larger", .default = try Chord.parse("cmd+=") });
    const resolved = try registry.resolve(&.{});
    try std.testing.expectEqual(@as(?usize, 1), resolved.match("{", cmd | shift));
    try std.testing.expectEqual(@as(?usize, 2), resolved.match("+", cmd | shift));
    try std.testing.expect(resolved.match("[", cmd | ctrl) == null);
    try std.testing.expect(resolved.match("[", cmd | alt) == null);
    try std.testing.expect(resolved.match("[", ctrl) == null);
}
