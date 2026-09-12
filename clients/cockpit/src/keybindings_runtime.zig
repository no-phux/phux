//! Runtime installation and discovery use the same registry as input fallback.
//! Instantiate State(native_sdk.platform); the generic boundary also lets tests
//! exercise replacement, failure, and dispatch without starting a live app.
const std = @import("std");
pub const bindings = @import("config/keybindings.zig");

pub fn State(comptime Platform: type) type {
    return struct {
        const Self = @This();
        const Registration = struct {
            resolved: bindings.Resolved = .{},
            // "kb." + u64 decimal + "." + at most three index digits.
            ids: [bindings.max_commands]bindings.Text(27) = @splat(.{}),
            shortcuts: [Platform.max_shortcuts]Platform.Shortcut = undefined,
            menus: [Platform.max_menus]Platform.Menu = undefined,
            items: [Platform.max_menu_items]Platform.MenuItem = undefined,
            shortcut_count: usize = 0,
        };
        registry: bindings.Registry = .{},
        original_menus: []const Platform.Menu = &.{},
        applied: bindings.Resolved = .{},
        installed: bool = false,
        enabled: bool = false,
        registrations: [2]Registration = @splat(.{}),
        active_bank: u1 = 0,
        generation: u64 = 0,

        pub fn init(shortcuts: []const Platform.Shortcut, menus: []const Platform.Menu) !Self {
            // Runtime configures the manifest before calling App.start. It is
            // the rollback target even if the first override install fails.
            var self: Self = .{ .original_menus = menus, .enabled = true };
            for (menus) |menu| {
                for (menu.items) |item| {
                    if (item.separator) continue;
                    try self.registry.add(.{ .id = item.command, .label = item.label, .default = try manifestChord(item.key, item.modifiers) });
                }
            }
            for (shortcuts) |shortcut| {
                const label = if (self.registry.indexOf(shortcut.id) == null) shortcutCaption(shortcut.id) else "";
                try self.registry.add(.{ .id = shortcut.id, .label = label, .default = try manifestChord(shortcut.key, shortcut.modifiers) });
            }
            self.applied = try self.registry.resolve(&.{});
            return self;
        }

        /// Keep State at a stable address after the first sync: NullPlatform
        /// copies descriptors but borrows their key/id slices. Two owned banks
        /// retain the previous installation while preparing its replacement.
        /// Suspending registrations AND menu equivalents preserves text editing.
        pub fn sync(self: *Self, services: anytype, overrides: *const bindings.Overrides, enabled: bool) !void {
            const candidate = try self.registry.resolve(overrides);
            if (self.installed and self.enabled == enabled and std.meta.eql(self.applied, candidate)) return;
            self.install(services, &candidate, enabled, self.active_bank ^ 1) catch |err| {
                // An attempt can publish shortcuts before failing on menus.
                // Rollback therefore uses a NEW generation, not the old IDs.
                self.install(services, &self.applied, self.enabled, self.active_bank) catch {
                    self.installed = false;
                    return error.KeybindingRollbackFailed;
                };
                return err;
            };
        }

        fn install(self: *Self, services: anytype, resolved: *const bindings.Resolved, enabled: bool, bank: u1) !void {
            const registration = &self.registrations[bank];
            try self.prepare(registration, resolved, enabled);
            self.installed = false;
            try services.configureShortcuts(registration.shortcuts[0..registration.shortcut_count]);
            try services.configureMenus(registration.menus[0..self.original_menus.len]);
            self.applied = registration.resolved;
            self.active_bank = bank;
            self.enabled = enabled;
            self.installed = true;
        }

        fn prepare(self: *Self, registration: *Registration, resolved: *const bindings.Resolved, enabled: bool) !void {
            self.generation = std.math.add(u64, self.generation, 1) catch return error.KeybindingGenerationExhausted;
            registration.resolved = resolved.*;
            registration.shortcut_count = try self.shortcutList(registration, enabled);
            try self.menuList(&registration.resolved, enabled, &registration.menus, &registration.items);
        }

        fn shortcutList(self: *Self, registration: *Registration, enabled: bool) !usize {
            if (!enabled) return 0;
            var count: usize = 0;
            const resolved = &registration.resolved;
            for (resolved.chords[0..resolved.count], 0..) |maybe, index| {
                const chord = maybe orelse continue;
                if (count == registration.shortcuts.len) return error.TooManyShortcuts;
                const id = &registration.ids[index];
                const text = try std.fmt.bufPrint(&id.bytes, "kb.{d}.{d}", .{ self.generation, index });
                id.len = text.len;
                registration.shortcuts[count] = .{ .id = id.slice(), .key = resolved.chords[index].?.key.slice(), .modifiers = platformModifiers(chord.modifiers) };
                count += 1;
            }
            return count;
        }

        fn menuList(self: *Self, resolved: *const bindings.Resolved, enabled: bool, menus: []Platform.Menu, items: []Platform.MenuItem) !void {
            if (self.original_menus.len > menus.len) return error.TooManyMenus;
            var offset: usize = 0;
            for (self.original_menus, 0..) |menu, index| {
                if (menu.items.len > items.len - offset) return error.TooManyMenuItems;
                const target = items[offset..][0..menu.items.len];
                for (menu.items, 0..) |item, item_index| target[item_index] = self.menuItem(item, resolved, enabled);
                menus[index] = .{ .title = menu.title, .items = target };
                offset += menu.items.len;
            }
        }

        fn menuItem(self: *Self, item: Platform.MenuItem, resolved: *const bindings.Resolved, enabled: bool) Platform.MenuItem {
            var result = item;
            result.key = "";
            result.modifiers = .{};
            if (!enabled) return result;
            const index = self.registry.indexOf(item.command) orelse return result;
            const chord = resolved.chords[index] orelse return result;
            result.key = resolved.chords[index].?.key.slice();
            result.modifiers = platformModifiers(chord.modifiers);
            return result;
        }

        pub fn commandForEvent(self: *const Self, event: anytype) ?[]const u8 {
            if (!self.installed or !self.enabled) return null;
            if (event.phase != .key_down) return null;
            const modifiers = widgetModifiers(event.modifiers);
            const index = self.applied.match(event.key, modifiers) orelse return null;
            return self.registry.commands[index].id;
        }

        /// A host event may have been queued before a remap. Never let its old
        /// command id bypass the current binding or execute in a text overlay.
        pub fn acceptsShortcut(self: *const Self, event: Platform.ShortcutEvent) bool {
            return self.commandForShortcut(event) != null;
        }

        /// Platform IDs name an installation, not a canonical command. A
        /// matched-registration event has already lost physical Shift, so its
        /// generation is the only proof it belongs to current input ownership.
        pub fn commandForShortcut(self: *const Self, event: Platform.ShortcutEvent) ?[]const u8 {
            if (!self.installed or !self.enabled) return null;
            const chord = manifestChord(event.key, event.modifiers) catch return null;
            const value = chord orelse return null;
            const index = self.applied.match(value.key.slice(), value.modifiers) orelse return null;
            const id = self.registrations[self.active_bank].ids[index].slice();
            if (!std.mem.eql(u8, id, event.id)) return null;
            return self.registry.commands[index].id;
        }

        pub fn response(self: *const Self, overrides: *const bindings.Overrides, notice: []const u8, out: []u8) ![]const u8 {
            if (self.installed) return encodeResponse(&self.registry, &self.applied, overrides, notice, out);
            const unknown: bindings.Resolved = .{ .count = self.registry.count };
            return encodeResponse(&self.registry, &unknown, overrides, "Shortcut installation is unknown. Retry to restore your bindings.", out);
        }

        fn platformModifiers(mask: u8) Platform.ShortcutModifiers {
            return .{ .command = mask & bindings.cmd != 0, .control = mask & bindings.ctrl != 0, .option = mask & bindings.alt != 0, .shift = mask & bindings.shift != 0 };
        }
    };
}

fn manifestChord(key: []const u8, modifiers: anytype) !?bindings.Chord {
    if (key.len == 0) return null;
    var mask: u8 = 0;
    if (modifiers.primary or modifiers.command) mask |= bindings.cmd;
    if (modifiers.control) mask |= bindings.ctrl;
    if (modifiers.option) mask |= bindings.alt;
    if (modifiers.shift) mask |= bindings.shift;
    return .{ .key = try bindings.normalizedKey(key), .modifiers = mask };
}

fn widgetModifiers(modifiers: anytype) u8 {
    var mask: u8 = 0;
    if (modifiers.super) mask |= bindings.cmd;
    if (modifiers.control) mask |= bindings.ctrl;
    if (modifiers.alt) mask |= bindings.alt;
    if (modifiers.shift) mask |= bindings.shift;
    return mask;
}

pub const max_response_bytes = 4 + 255 + bindings.max_commands * (6 + bindings.max_id_bytes + 128 + 2 * bindings.max_chord_bytes);

pub const Request = struct {
    action: u8,
    index: usize,
    value: []const u8,

    pub fn decode(bytes: []const u8) !Request {
        if (bytes.len < 4 or bytes[0] != 1) return error.InvalidRequest;
        if (bytes[1] > 3 or bytes[3] > bindings.max_chord_bytes) return error.InvalidRequest;
        if (bytes.len != 4 + @as(usize, bytes[3])) return error.InvalidRequest;
        if (bytes[1] != 1 and bytes[3] != 0) return error.InvalidRequest;
        return .{ .action = bytes[1], .index = bytes[2], .value = bytes[4..] };
    }
};

/// Encode only the currently accepted registry, never an unapplied candidate.
/// A rejected edit can still return the real rows with an actionable notice.
pub const response = encodeResponse;

fn encodeResponse(registry: *const bindings.Registry, resolved: *const bindings.Resolved, overrides: *const bindings.Overrides, notice: []const u8, out: []u8) ![]const u8 {
    if (notice.len > 255) return error.NoticeTooLong;
    var writer: Writer = .{ .out = out };
    try writer.write(&.{ 1, @intCast(registry.count), @intFromBool(notice.len > 0), @intCast(notice.len) });
    try writer.write(notice);
    for (registry.commands[0..registry.count], 0..) |command, index| {
        try writeRow(&writer, command, index, resolved.chords[index], overrides.indexOf(command.id) != null);
    }
    return out[0..writer.offset];
}

fn writeRow(writer: *Writer, command: bindings.Command, index: usize, effective: ?bindings.Chord, overridden: bool) !void {
    const current = chordText(effective);
    const default = chordText(command.default);
    const label = if (command.label.len > 0) command.label else command.id;
    if (label.len > 128) return error.LabelTooLong;
    try writer.write(&.{ @intCast(index), @intFromBool(overridden), @intCast(command.id.len), @intCast(label.len), @intCast(current.len), @intCast(default.len) });
    try writer.write(command.id);
    try writer.write(label);
    try writer.write(current.slice());
    try writer.write(default.slice());
}

fn chordText(chord: ?bindings.Chord) bindings.Text(bindings.max_chord_bytes) {
    return if (chord) |value| value.format() else .{};
}

const Writer = struct {
    out: []u8,
    offset: usize = 0,
    fn write(self: *Writer, bytes: []const u8) !void {
        if (bytes.len > self.out.len - self.offset) return error.NoSpaceLeft;
        @memcpy(self.out[self.offset..][0..bytes.len], bytes);
        self.offset += bytes.len;
    }
};

pub fn errorNotice(err: anyerror) []const u8 {
    return switch (err) {
        error.ConflictingChord => "This chord is already assigned. Clear its other binding first, then retry.",
        error.TerminalOwnedChord => "Include Cmd: unmodified, Control and Option keys belong to terminal applications.",
        error.ReservedChord => "macOS or the application menu owns this chord. Choose another key.",
        error.UnknownCommand => "This command is not in the current application. Reload the binding list.",
        error.InvalidModifier, error.DuplicateModifier, error.InvalidKey => "Use a chord such as Cmd+Shift+p, or none to remove a binding.",
        error.KeybindingRollbackFailed => "Shortcut restoration failed. Retry to repair the installation before using app shortcuts.",
        else => "Could not apply these bindings. Your previous bindings remain active; retry or cancel.",
    };
}

/// Canonical captions agreed with the frontend command catalog. They annotate
/// only IDs actually present in app.zon; they never create executable commands.
fn shortcutCaption(id: []const u8) []const u8 {
    const captions = .{
        .{ "surface.1", "Select Tab 1" },
        .{ "surface.2", "Select Tab 2" },
        .{ "surface.3", "Select Tab 3" },
        .{ "surface.4", "Select Tab 4" },
        .{ "surface.5", "Select Tab 5" },
        .{ "pane.focus-left", "Focus Pane Left" },
        .{ "pane.focus-right", "Focus Pane Right" },
        .{ "pane.focus-up", "Focus Pane Up" },
        .{ "pane.focus-down", "Focus Pane Down" },
    };
    inline for (captions) |caption| {
        if (std.mem.eql(u8, id, caption[0])) return caption[1];
    }
    return "";
}

const TestPlatform = struct {
    const max_shortcuts = 64;
    const max_menus = 16;
    const max_menu_items = 128;
    const max_shortcut_id_bytes = 64;
    const ShortcutModifiers = struct {
        primary: bool = false,
        command: bool = false,
        control: bool = false,
        option: bool = false,
        shift: bool = false,
    };
    const Shortcut = struct { id: []const u8, key: []const u8, modifiers: ShortcutModifiers = .{} };
    const ShortcutEvent = Shortcut;
    const MenuItem = struct { command: []const u8 = "", label: []const u8 = "", key: []const u8 = "", modifiers: ShortcutModifiers = .{}, separator: bool = false };
    const Menu = struct { title: []const u8, items: []const MenuItem };
};

const TestServices = struct {
    registry: bindings.Registry = .{},
    menu_key: bindings.Text(32) = .{},
    menu_calls: usize = 0,
    shortcut_calls: usize = 0,
    fail_menu_once: bool = false,
    fail_shortcut_call: usize = 0,

    fn configureShortcuts(self: *TestServices, shortcuts: []const TestPlatform.Shortcut) !void {
        self.shortcut_calls += 1;
        if (self.shortcut_calls == self.fail_shortcut_call) return error.TestShortcutsFailed;
        self.registry = .{};
        for (shortcuts) |shortcut| {
            try self.registry.add(.{ .id = shortcut.id, .default = try manifestChord(shortcut.key, shortcut.modifiers) });
        }
    }
    fn configureMenus(self: *TestServices, menus: []const TestPlatform.Menu) !void {
        self.menu_calls += 1;
        if (self.fail_menu_once) {
            self.fail_menu_once = false;
            return error.TestMenuFailed;
        }
        self.menu_key = try bindings.Text(32).init(menus[0].items[0].key);
    }
    fn dispatch(self: *const TestServices, state: *const State(TestPlatform), key: []const u8, modifiers: u8) ?[]const u8 {
        const resolved = self.registry.defaults();
        const index = resolved.match(key, modifiers) orelse return null;
        const chord = resolved.chords[index].?;
        return state.commandForShortcut(.{ .id = self.registry.commands[index].id, .key = chord.key.slice(), .modifiers = State(TestPlatform).platformModifiers(chord.modifiers) });
    }
};

const test_menus = [_]TestPlatform.Menu{.{ .title = "Shell", .items = &.{
    .{ .command = "terminal.new", .label = "New Tab", .key = "t", .modifiers = .{ .primary = true } },
    .{ .command = "window.new", .label = "New Window", .key = "n", .modifiers = .{ .primary = true } },
} }};

test "runtime remap replaces actual platform dispatch and menu equivalents once" {
    var state = try State(TestPlatform).init(&.{}, &test_menus);
    var services: TestServices = .{};
    var overrides: bindings.Overrides = .{};
    try state.sync(&services, &overrides, true);
    try std.testing.expectEqualStrings("terminal.new", services.dispatch(&state, "t", bindings.cmd).?);
    try std.testing.expect(services.dispatch(&state, "r", bindings.cmd) == null);
    try overrides.set("terminal.new", "cmd+r");
    try state.sync(&services, &overrides, true);
    try std.testing.expect(services.dispatch(&state, "t", bindings.cmd) == null);
    try std.testing.expectEqualStrings("terminal.new", services.dispatch(&state, "r", bindings.cmd).?);
    try std.testing.expectEqualStrings("r", services.menu_key.slice());
    try std.testing.expect(services.dispatch(&state, "r", bindings.ctrl) == null);
    try std.testing.expect(services.dispatch(&state, "r", bindings.cmd | bindings.alt) == null);
    try state.sync(&services, &overrides, true);
    try std.testing.expectEqual(@as(usize, 2), services.shortcut_calls);
    try std.testing.expectEqual(@as(usize, 2), services.menu_calls);
}

test "text editing suspends capture and reset restores default physical dispatch" {
    var state = try State(TestPlatform).init(&.{}, &test_menus);
    var services: TestServices = .{};
    var overrides: bindings.Overrides = .{};
    try overrides.set("terminal.new", "cmd+r");
    try state.sync(&services, &overrides, true);
    try state.sync(&services, &overrides, false);
    try std.testing.expect(services.dispatch(&state, "r", bindings.cmd) == null);
    try std.testing.expectEqualStrings("", services.menu_key.slice());
    overrides.resetAll();
    try state.sync(&services, &overrides, true);
    try std.testing.expectEqualStrings("terminal.new", services.dispatch(&state, "t", bindings.cmd).?);
    try std.testing.expect(services.dispatch(&state, "r", bindings.cmd) == null);
    try std.testing.expectEqualStrings("t", services.menu_key.slice());
}

test "failed menu replacement rolls shortcuts back before publishing new hints" {
    var state = try State(TestPlatform).init(&.{}, &test_menus);
    var services: TestServices = .{};
    var overrides: bindings.Overrides = .{};
    try state.sync(&services, &overrides, true);
    try overrides.set("terminal.new", "cmd+r");
    services.fail_menu_once = true;
    try std.testing.expectError(error.TestMenuFailed, state.sync(&services, &overrides, true));
    try std.testing.expectEqualStrings("terminal.new", services.dispatch(&state, "t", bindings.cmd).?);
    try std.testing.expect(services.dispatch(&state, "r", bindings.cmd) == null);
    try std.testing.expectEqualStrings("t", services.menu_key.slice());
    try std.testing.expectEqualStrings("t", state.applied.chords[0].?.key.slice());
}

test "request validation and response snapshot describe accepted bindings" {
    try std.testing.expectError(error.InvalidRequest, Request.decode(&.{ 1, 2, 0, 1, 'x' }));
    const request = try Request.decode(&.{ 1, 1, 0, 4, 'n', 'o', 'n', 'e' });
    var state = try State(TestPlatform).init(&.{}, &test_menus);
    var overrides: bindings.Overrides = .{};
    try state.registry.edit(&overrides, request.action, request.index, request.value);
    const resolved = try state.registry.resolve(&overrides);
    var bytes: [max_response_bytes]u8 = undefined;
    const result = try response(&state.registry, &resolved, &overrides, "", &bytes);
    try std.testing.expectEqualSlices(u8, &.{ 1, 2, 0, 0, 0, 1, 12, 7, 0, 5 }, result[0..10]);
    try std.testing.expectError(error.NoSpaceLeft, response(&state.registry, &resolved, &overrides, "", bytes[0..10]));
}

test "compound replacement failure invalidates installation and forces repair" {
    var state = try State(TestPlatform).init(&.{}, &test_menus);
    var services: TestServices = .{};
    const original: bindings.Overrides = .{};
    try state.sync(&services, &original, true);
    const original_id = try bindings.Text(27).init(services.registry.commands[0].id);
    var changed = original;
    try changed.set("terminal.new", "cmd+r");
    services.fail_menu_once = true;
    services.fail_shortcut_call = 3; // Candidate shortcuts succeeded; rollback fails.
    try std.testing.expectError(error.KeybindingRollbackFailed, state.sync(&services, &changed, true));
    try std.testing.expect(!state.installed);
    const candidate_id = try bindings.Text(27).init(services.registry.commands[0].id);
    // The physical registration is still the candidate, but neither command
    // acceptance nor displayed hints may claim a known installation.
    try std.testing.expect(services.registry.defaults().match("r", bindings.cmd) != null);
    try std.testing.expect(services.dispatch(&state, "r", bindings.cmd) == null);
    try std.testing.expect(!state.acceptsShortcut(.{ .id = original_id.slice(), .key = "t", .modifiers = .{ .command = true } }));
    var bytes: [max_response_bytes]u8 = undefined;
    const unknown = try state.response(&original, "", &bytes);
    try std.testing.expectEqual(@as(u8, 1), unknown[2]);
    try std.testing.expectEqual(@as(u8, 0), unknown[4 + @as(usize, unknown[3]) + 4]);
    try state.sync(&services, &original, true);
    try std.testing.expect(services.shortcut_calls > 3);
    try std.testing.expectEqualStrings("t", state.applied.chords[0].?.key.slice());
    try std.testing.expectEqualStrings("t", services.menu_key.slice());
    try std.testing.expectEqualStrings("terminal.new", services.dispatch(&state, "t", bindings.cmd).?);
    try std.testing.expect(!state.acceptsShortcut(.{ .id = original_id.slice(), .key = "t", .modifiers = .{ .command = true } }));
    try std.testing.expect(!state.acceptsShortcut(.{ .id = candidate_id.slice(), .key = "r", .modifiers = .{ .command = true } }));
    const repaired = try state.response(&original, "", &bytes);
    try std.testing.expectEqual(@as(u8, 0), repaired[2]);
    try std.testing.expectEqual(@as(u8, 5), repaired[8]);
}

test "rollback restores keys under a new generation without reviving queued IDs" {
    var state = try State(TestPlatform).init(&.{}, &test_menus);
    var services: TestServices = .{};
    var overrides: bindings.Overrides = .{};
    try state.sync(&services, &overrides, true);
    const old_id = try bindings.Text(27).init(services.registry.commands[0].id);
    try overrides.set("terminal.new", "cmd+r");
    services.fail_menu_once = true;
    try std.testing.expectError(error.TestMenuFailed, state.sync(&services, &overrides, true));
    try std.testing.expect(state.installed);
    try std.testing.expectEqual(@as(u64, 3), state.generation);
    try std.testing.expect(!state.acceptsShortcut(.{ .id = old_id.slice(), .key = "t", .modifiers = .{ .command = true } }));
    try std.testing.expectEqualStrings("terminal.new", services.dispatch(&state, "t", bindings.cmd).?);
}
