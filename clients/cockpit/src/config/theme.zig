//! Built-in colour themes and the WCAG contrast arithmetic.
//!
//! A theme sets only `background`, `foreground` and `selection_background`,
//! which reach the terminal through the design tokens rebuilt every frame, so
//! a theme change repaints live. Explicit keys outrank it. It deliberately
//! does not set the ANSI-16 palette or cursor: those live in the emulator,
//! and switching themes could not un-set a palette slot.

const std = @import("std");

/// A colour, byte per channel. Structurally identical to `config.Rgb` and
/// deliberately declared HERE rather than imported from there: `config.zig`
/// imports this module for theme lookup, and the reverse import would be a
/// cycle.
pub const Rgb = struct {
    r: u8,
    g: u8,
    b: u8,
};

pub const Theme = struct {
    /// The name `theme = <name>` matches, case-insensitively.
    name: []const u8,
    /// One line for the settings surface. Present tense, no marketing.
    summary: []const u8,
    background: Rgb,
    foreground: Rgb,
    selection_background: Rgb,
};

/// The built-in set, in settings order. `phux-dark` restates the app's
/// unthemed tokens so selecting it is a no-op. Every entry clears WCAG AA
/// (4.5:1), pinned by a test.
pub const builtins = [_]Theme{
    .{
        .name = "phux-dark",
        .summary = "The app's own register: porcelain on deep graphite",
        .background = .{ .r = 0x09, .g = 0x0b, .b = 0x0f },
        .foreground = .{ .r = 0xf4, .g = 0xf7, .b = 0xfb },
        .selection_background = .{ .r = 0xbe, .g = 0xf2, .b = 0x64 },
    },
    .{
        .name = "phux-light",
        .summary = "The same register inverted, for a bright room",
        .background = .{ .r = 0xfb, .g = 0xfc, .b = 0xfe },
        .foreground = .{ .r = 0x10, .g = 0x14, .b = 0x1b },
        .selection_background = .{ .r = 0x84, .g = 0xcc, .b = 0x16 },
    },
    .{
        .name = "high-contrast",
        .summary = "Pure white on pure black, the maximum this display can do",
        .background = .{ .r = 0x00, .g = 0x00, .b = 0x00 },
        .foreground = .{ .r = 0xff, .g = 0xff, .b = 0xff },
        .selection_background = .{ .r = 0xff, .g = 0xd4, .b = 0x00 },
    },
    .{
        .name = "nord",
        .summary = "Cool arctic greys",
        .background = .{ .r = 0x2e, .g = 0x34, .b = 0x40 },
        .foreground = .{ .r = 0xd8, .g = 0xde, .b = 0xe9 },
        .selection_background = .{ .r = 0x88, .g = 0xc0, .b = 0xd0 },
    },
    .{
        .name = "gruvbox-dark",
        .summary = "Warm retro cream on charcoal",
        .background = .{ .r = 0x28, .g = 0x28, .b = 0x28 },
        .foreground = .{ .r = 0xeb, .g = 0xdb, .b = 0xb2 },
        .selection_background = .{ .r = 0xd7, .g = 0x99, .b = 0x21 },
    },
    .{
        .name = "solarized-dark",
        .summary = "The classic low-glare blue-green",
        .background = .{ .r = 0x00, .g = 0x2b, .b = 0x36 },
        .foreground = .{ .r = 0x93, .g = 0xa1, .b = 0xa1 },
        .selection_background = .{ .r = 0x26, .g = 0x8b, .b = 0xd2 },
    },
};

/// `theme = auto`: follow the system between `phux-dark` and `phux-light`
/// (the same register inverted).
pub const auto_name = "auto";
pub const auto_dark = "phux-dark";
pub const auto_light = "phux-light";

/// The platform's own light/dark answer, restated here so `config.zig` — which
/// cannot see the SDK — can take one without importing it.
pub const ColorScheme = enum { light, dark };

pub fn isAutoName(name: []const u8) bool {
    return std.ascii.eqlIgnoreCase(name, auto_name);
}

/// The theme name a system appearance selects. Total by construction: there is
/// no third scheme, and a missing member of the pair would be a compile error
/// through `builtins` below rather than a runtime fallback.
pub fn forScheme(scheme: ColorScheme) []const u8 {
    return switch (scheme) {
        .dark => auto_dark,
        .light => auto_light,
    };
}

comptime {
    // The pair has to name real themes. Both are built in above, and a rename
    // that broke this would otherwise surface as a `theme = auto` that
    // silently stopped changing anything.
    if (byNameComptime(auto_dark) == null) @compileError("theme.auto_dark names no built-in theme");
    if (byNameComptime(auto_light) == null) @compileError("theme.auto_light names no built-in theme");
}

fn byNameComptime(comptime name: []const u8) ?*const Theme {
    for (&builtins) |*theme| {
        if (std.mem.eql(u8, theme.name, name)) return theme;
    }
    return null;
}

/// The theme a name selects, or null for a name this build does not ship.
/// Case-insensitive, matching every other value the config parser accepts.
pub fn byName(name: []const u8) ?*const Theme {
    for (&builtins) |*theme| {
        if (std.ascii.eqlIgnoreCase(theme.name, name)) return theme;
    }
    return null;
}

/// The index of a named theme in `builtins`, for a settings cursor that has to
/// open ON the theme already in effect rather than at row zero.
pub fn indexOf(name: []const u8) ?usize {
    for (&builtins, 0..) |*theme, index| {
        if (std.ascii.eqlIgnoreCase(theme.name, name)) return index;
    }
    return null;
}

// ------------------------------------------------------------------ contrast

/// The WCAG AA contrast minimum for BODY text (Success Criterion 1.4.3,
/// Contrast (Minimum)). 3:1 is the large-text allowance and does not apply
/// here: a terminal is body text at every size anyone reads it at.
pub const wcag_aa_body_text: f32 = 4.5;
/// The AAA minimum (SC 1.4.6), reported but never used as the pass/fail line.
pub const wcag_aaa_body_text: f32 = 7.0;

/// One sRGB channel (0..1), linearized with WCAG 2.x's published 0.03928 knee.
fn linearize(channel: f32) f32 {
    if (channel <= 0.03928) return channel / 12.92;
    return std.math.pow(f32, (channel + 0.055) / 1.055, 2.4);
}

/// WCAG 2.x relative luminance of normalized sRGB channels (the canvas's own
/// f32 colours, so the painted colour is what gets measured).
pub fn relativeLuminance(r: f32, g: f32, b: f32) f32 {
    return 0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b);
}

pub fn relativeLuminanceRgb(color: Rgb) f32 {
    return relativeLuminance(
        @as(f32, @floatFromInt(color.r)) / 255.0,
        @as(f32, @floatFromInt(color.g)) / 255.0,
        @as(f32, @floatFromInt(color.b)) / 255.0,
    );
}

/// WCAG 2.x contrast ratio between two luminances; symmetric, 1.0 to 21.0.
pub fn contrastRatioLuminance(a: f32, b: f32) f32 {
    const lighter = @max(a, b);
    const darker = @min(a, b);
    return (lighter + 0.05) / (darker + 0.05);
}

pub fn contrastRatio(a: Rgb, b: Rgb) f32 {
    return contrastRatioLuminance(relativeLuminanceRgb(a), relativeLuminanceRgb(b));
}

/// How the readout GRADES a ratio. Three bands rather than a bare number,
/// because a number alone still needs the reader to remember 4.5.
pub const Legibility = enum {
    /// Below 4.5:1. This is the state the owner has been staring at for four
    /// rounds without a way to name it.
    fails_aa,
    /// 4.5:1 or better, below 7:1.
    passes_aa,
    /// 7:1 or better.
    passes_aaa,

    pub fn of(ratio: f32) Legibility {
        if (ratio < wcag_aa_body_text) return .fails_aa;
        if (ratio < wcag_aaa_body_text) return .passes_aa;
        return .passes_aaa;
    }

    pub fn readable(self: Legibility) bool {
        return self != .fails_aa;
    }
};
