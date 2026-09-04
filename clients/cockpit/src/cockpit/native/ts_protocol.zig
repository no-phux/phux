const std = @import("std");

/// The entire app-owned interface between the compiled TypeScript core and
/// Cockpit's native engine. Intent payloads and snapshots are versioned binary
/// records behind these names; terminal bytes never cross this seam.
pub const intent_command = "cockpit.intent";
pub const snapshot_request = "cockpit.snapshot";

/// "COCK" plus a low tag, kept below JavaScript's largest exact integer.
pub const event_channel_key: u64 = 0x434f_434b_0001;
pub const version: u8 = 1;

pub const EventKind = enum(u8) {
    state_invalidated = 1,
    snapshot = 2,
};

pub const IntentKind = enum(u8) {
    select_tab = 1,
    new_terminal = 2,
    close_tab = 3,
    set_tab_placement = 4,
    /// Apply and persist the theme at `argument` in the builtin catalog.
    set_theme = 5,
    /// Reveal the active configuration file in the OS file browser.
    reveal_config = 6,
    /// Ask the engine whether the config file exists and will take a write,
    /// once, when the settings surface opens: a probe touches the disk, and
    /// a snapshot must not.
    probe_config = 7,
    /// A new window with one shell, as cmd+N; `window` is ignored.
    new_window = 8,
    /// Close window `window` whole, as the OS close button; every tab's
    /// shells go with it.
    close_window = 9,
    /// Make window `window` the active one, as the OS focus did.
    focus_window = 10,
    /// Execute one command from the shared native menu/shortcut vocabulary.
    /// `argument` is a `NativeCommand`; `window` is the window that produced
    /// the platform event, already reduced to a process-local projection slot.
    native_command = 11,
};

pub const NativeCommand = enum(u8) {
    previous_tab = 1,
    next_tab = 2,
    close_focused_pane = 3,
    split_right = 4,
    split_down = 5,
    previous_pane = 6,
    next_pane = 7,
    move_tab_left = 8,
    move_tab_right = 9,
    select_all = 10,
    copy = 11,
    paste = 12,
    clear = 13,
    find = 14,
    find_next = 15,
    find_previous = 16,
    font_larger = 17,
    font_smaller = 18,
    font_reset = 19,
    focus_left = 20,
    focus_right = 21,
    focus_up = 22,
    focus_down = 23,
    fullscreen = 24,
    minimize = 25,
};

pub fn decodeNativeCommand(raw: u8) ?NativeCommand {
    return switch (raw) {
        1 => .previous_tab,
        2 => .next_tab,
        3 => .close_focused_pane,
        4 => .split_right,
        5 => .split_down,
        6 => .previous_pane,
        7 => .next_pane,
        8 => .move_tab_left,
        9 => .move_tab_right,
        10 => .select_all,
        11 => .copy,
        12 => .paste,
        13 => .clear,
        14 => .find,
        15 => .find_next,
        16 => .find_previous,
        17 => .font_larger,
        18 => .font_smaller,
        19 => .font_reset,
        20 => .focus_left,
        21 => .focus_right,
        22 => .focus_up,
        23 => .focus_down,
        24 => .fullscreen,
        25 => .minimize,
        else => null,
    };
}

pub const invalidation_len: usize = 18;
pub const snapshot_header_len: usize = invalidation_len;

pub const Invalidation = struct {
    sequence: u64,
    revision: u64,
};

pub const intent_len: usize = 12;

/// `window` addresses the window an intent means (0 is the main window);
/// a tab intent from a secondary window's chrome names that window so it
/// cannot land on whichever window happened to be active.
pub const Intent = struct {
    kind: IntentKind,
    expected_revision: u64,
    argument: u8,
    window: u8 = 0,
};

pub fn encodeInvalidation(sequence: u64, revision: u64) [invalidation_len]u8 {
    var out: [invalidation_len]u8 = undefined;
    out[0] = version;
    out[1] = @intFromEnum(EventKind.state_invalidated);
    std.mem.writeInt(u64, out[2..10], sequence, .little);
    std.mem.writeInt(u64, out[10..18], revision, .little);
    return out;
}

pub fn decodeInvalidation(bytes: []const u8) ?Invalidation {
    if (bytes.len != invalidation_len) return null;
    if (bytes[0] != version) return null;
    if (bytes[1] != @intFromEnum(EventKind.state_invalidated)) return null;
    return .{
        .sequence = std.mem.readInt(u64, bytes[2..10], .little),
        .revision = std.mem.readInt(u64, bytes[10..18], .little),
    };
}

pub fn encodeSnapshotHeader(sequence: u64, revision: u64) [snapshot_header_len]u8 {
    var out = encodeInvalidation(sequence, revision);
    out[1] = @intFromEnum(EventKind.snapshot);
    return out;
}

pub fn encodeIntent(value: Intent) [intent_len]u8 {
    var out: [intent_len]u8 = undefined;
    out[0] = version;
    out[1] = @intFromEnum(value.kind);
    std.mem.writeInt(u64, out[2..10], value.expected_revision, .little);
    out[10] = value.argument;
    out[11] = value.window;
    return out;
}

pub fn decodeIntent(bytes: []const u8) ?Intent {
    if (bytes.len != intent_len or bytes[0] != version) return null;
    const kind: IntentKind = switch (bytes[1]) {
        1 => .select_tab,
        2 => .new_terminal,
        3 => .close_tab,
        4 => .set_tab_placement,
        5 => .set_theme,
        6 => .reveal_config,
        7 => .probe_config,
        8 => .new_window,
        9 => .close_window,
        10 => .focus_window,
        11 => .native_command,
        else => return null,
    };
    return .{
        .kind = kind,
        .expected_revision = std.mem.readInt(u64, bytes[2..10], .little),
        .argument = bytes[10],
        .window = bytes[11],
    };
}

test "the TypeScript engine invalidation packet is fixed and versioned" {
    const bytes = encodeInvalidation(0x0807_0605_0403_0201, 0x8877_6655_4433_2211);
    try std.testing.expectEqualSlices(u8, &.{
        version, 1,
        0x01,    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        0x11,    0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
    }, &bytes);
    const decoded = decodeInvalidation(&bytes).?;
    try std.testing.expectEqual(@as(u64, 0x0807_0605_0403_0201), decoded.sequence);
    try std.testing.expectEqual(@as(u64, 0x8877_6655_4433_2211), decoded.revision);

    var wrong_version = bytes;
    wrong_version[0] +%= 1;
    try std.testing.expect(decodeInvalidation(&wrong_version) == null);
    try std.testing.expect(decodeInvalidation(bytes[0 .. bytes.len - 1]) == null);
}

test "the TypeScript channel key stays exactly representable" {
    try std.testing.expect(event_channel_key > 0);
    try std.testing.expect(event_channel_key < 9_007_199_254_740_992);
}

test "snapshot headers share sequence and revision framing without masquerading as events" {
    const bytes = encodeSnapshotHeader(21, 34);
    try std.testing.expectEqual(@as(u8, 2), bytes[1]);
    try std.testing.expect(decodeInvalidation(&bytes) == null);
    try std.testing.expectEqual(@as(u64, 21), std.mem.readInt(u64, bytes[2..10], .little));
    try std.testing.expectEqual(@as(u64, 34), std.mem.readInt(u64, bytes[10..18], .little));
}

test "TypeScript intents fence positional arguments with the engine revision" {
    const value: Intent = .{
        .kind = .close_tab,
        .expected_revision = 0x8877_6655_4433_2211,
        .argument = 14,
        .window = 2,
    };
    const bytes = encodeIntent(value);
    try std.testing.expectEqualSlices(u8, &.{
        version, 3,
        0x11,    0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        14,      2,
    }, &bytes);
    const decoded = decodeIntent(&bytes).?;
    try std.testing.expectEqual(value.kind, decoded.kind);
    try std.testing.expectEqual(value.expected_revision, decoded.expected_revision);
    try std.testing.expectEqual(value.argument, decoded.argument);
    try std.testing.expectEqual(value.window, decoded.window);

    var unknown = bytes;
    unknown[1] = 255;
    try std.testing.expect(decodeIntent(&unknown) == null);
}
