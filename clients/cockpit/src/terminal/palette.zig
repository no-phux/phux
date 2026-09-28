//! Emulator-color resolution for terminal grid projection.

const std = @import("std");
const native_sdk = @import("native_sdk");
const vt = @import("ghostty-vt");
const theme_module = @import("../config/theme.zig");

const canvas = native_sdk.canvas;

/// A theme token color (f32 rgba 0..1) as the emulator's 8-bit RGB.
pub fn themeRgb(color: canvas.Color) vt.color.RGB {
    return .{
        .r = @intFromFloat(std.math.clamp(color.r, 0, 1) * 255 + 0.5),
        .g = @intFromFloat(std.math.clamp(color.g, 0, 1) * 255 + 0.5),
        .b = @intFromFloat(std.math.clamp(color.b, 0, 1) * 255 + 0.5),
    };
}

fn rgbToColor(rgb: vt.color.RGB) canvas.Color {
    return canvas.Color.rgb8(rgb.r, rgb.g, rgb.b);
}

/// The emulator's colour state, resolved for the painter. The 256-entry
/// palette is the emulator's own (a terminal red is not a UI accent); only
/// fg/bg/cursor are theme-derived, as emulator defaults OSC 10/11/12 can
/// still override.
pub const Palette = struct {
    background: canvas.Color,
    foreground: canvas.Color,
    cursor: canvas.Color,
    selection: canvas.Color,
    /// The wash under EVERY scrollback-search match, and the text color that
    /// reads on it.
    search_match: canvas.Color,
    search_match_text: canvas.Color,
    /// The wash under the ONE match the user is standing on.
    search_current: canvas.Color,
    search_current_text: canvas.Color,
    terminal: *const vt.RenderState.Colors,
    /// The emulator's live 256-color palette, overrides applied. Read directly
    /// (rather than mirrored into a local array) so OSC 4 lands the same frame.
    dynamic: *const vt.color.DynamicPalette,
    /// The WCAG ratio a resolved foreground must clear against the background
    /// it lands on, or 1 for "no floor". See `contrasted`.
    minimum_contrast: f32 = 1,

    pub fn init(
        tokens: canvas.DesignTokens,
        terminal_colors: *const vt.RenderState.Colors,
        dynamic: *const vt.color.DynamicPalette,
        minimum_contrast: f32,
    ) Palette {
        const colors = tokens.colors;
        // Resolved render colors already include theme defaults, OSC overrides,
        // and DECSCNM reverse swap.
        const background = rgbToColor(terminal_colors.background);
        const foreground = rgbToColor(terminal_colors.foreground);
        // Matches are amber pulled toward the ground; the current match uses
        // the accent, a different hue, so it stands out on a dense screen.
        const match = blend(colors.warning, background, 0.45);
        const current = colors.accent;
        return .{
            .background = background,
            .foreground = foreground,
            .cursor = if (terminal_colors.cursor) |cur| rgbToColor(cur) else colors.accent,
            .selection = colors.accent,
            .search_match = match,
            .search_match_text = readableOver(match, foreground, background),
            .search_current = current,
            .search_current_text = readableOver(current, foreground, background),
            .terminal = terminal_colors,
            .dynamic = dynamic,
            .minimum_contrast = minimum_contrast,
        };
    }

    pub fn indexed(palette: *const Palette, index: u8) canvas.Color {
        const live = palette.dynamic.current[index];
        return canvas.Color.rgb8(live.r, live.g, live.b);
    }

    /// The glyph colour with everything applied. `bg` is the cell's own
    /// background (null for the ground); the contrast floor needs it and `cp`.
    pub fn resolveFg(palette: *const Palette, style: vt.Style, bg: ?canvas.Color, cp: u21) canvas.Color {
        // The floor applies last, to the colours that actually meet on glass
        // (after inverse and faint), as Ghostty does.
        var color = if (style.flags.inverse) palette.resolveBgRaw(style) else fg: {
            var raw = palette.resolveFgRaw(style);
            if (style.flags.faint) raw = blend(raw, palette.background, 0.5);
            break :fg raw;
        };
        // Against the painted background, never a recomputed one.
        color = contrasted(palette.minimum_contrast, color, bg orelse palette.background, cp);
        return color;
    }

    /// Ghostty's `contrasted_color`: when `fg` falls short of the WCAG floor
    /// against `bg`, replace it outright with whichever of white or black
    /// scores higher (a snap, not a hue nudge). Graphics code points are
    /// exempt (see `noMinimumContrast`). The ratio maths is shared with
    /// `config/theme.zig` so the settings readout agrees with the renderer.
    pub fn contrasted(minimum_contrast: f32, fg: canvas.Color, bg: canvas.Color, cp: u21) canvas.Color {
        // Negated so a NaN floor disables the check instead of repainting
        // every cell.
        if (!(minimum_contrast > 1)) return fg;
        if (noMinimumContrast(cp)) return fg;

        const bg_luminance = theme_module.relativeLuminance(bg.r, bg.g, bg.b);
        const fg_luminance = theme_module.relativeLuminance(fg.r, fg.g, fg.b);
        if (theme_module.contrastRatioLuminance(fg_luminance, bg_luminance) >= minimum_contrast) {
            return fg;
        }

        // `relativeLuminance` of pure white is 1 and of pure black is 0 by
        // construction, so the two candidate ratios need no second linearize.
        const white_ratio = theme_module.contrastRatioLuminance(1, bg_luminance);
        const black_ratio = theme_module.contrastRatioLuminance(0, bg_luminance);
        return if (white_ratio > black_ratio)
            canvas.Color.rgb8(255, 255, 255)
        else
            canvas.Color.rgb8(0, 0, 0);
    }

    pub fn resolveFgRaw(palette: *const Palette, style: vt.Style) canvas.Color {
        return switch (style.fg_color) {
            .none => palette.foreground,
            // Bold-as-bright over ANSI 0-7 only (a colour decision; the
            // `bold` flag separately requests weight).
            .palette => |index| palette.indexed(
                if (style.flags.bold and index < 8) index + 8 else index,
            ),
            .rgb => |rgb| canvas.Color.rgb8(rgb.r, rgb.g, rgb.b),
        };
    }

    /// SGR 58's underline colour, or null (the foreground).
    pub fn resolveUnderlineColor(palette: *const Palette, style: vt.Style) ?canvas.Color {
        return switch (style.underline_color) {
            .none => null,
            .palette => |index| palette.indexed(index),
            .rgb => |rgb| canvas.Color.rgb8(rgb.r, rgb.g, rgb.b),
        };
    }

    fn resolveBgRaw(palette: *const Palette, style: vt.Style) canvas.Color {
        return switch (style.bg_color) {
            .none => palette.background,
            .palette => |index| palette.indexed(index),
            .rgb => |rgb| canvas.Color.rgb8(rgb.r, rgb.g, rgb.b),
        };
    }
};

pub fn cellBackground(cell: anytype, palette: *const Palette) ?canvas.Color {
    switch (cell.raw.content_tag) {
        .bg_color_palette => return palette.indexed(cell.raw.content.color_palette.data),
        .bg_color_rgb => {
            const rgb = cell.raw.content.color_rgb;
            return canvas.Color.rgb8(rgb.r, rgb.g, rgb.b);
        },
        else => {},
    }
    if (cell.raw.style_id == 0) return null;
    const style = cell.style;
    if (style.flags.inverse) return palette.resolveFgRaw(style);
    return switch (style.bg_color) {
        .none => null,
        .palette => |index| palette.indexed(index),
        .rgb => |rgb| canvas.Color.rgb8(rgb.r, rgb.g, rgb.b),
    };
}

/// Graphics code points exempt from the contrast floor, as Ghostty's
/// `noMinContrast`: box drawing, block elements, legacy computing symbols and
/// Powerline, whose foreground colour is the design.
fn noMinimumContrast(cp: u21) bool {
    return switch (cp) {
        0x2500...0x257F => true, // box drawing
        0x2580...0x259F => true, // block elements
        0xE0B0...0xE0D7 => true, // powerline
        0x1CC00...0x1CEBF => true, // legacy computing supplement
        0x1FB00...0x1FBFF => true, // legacy computing
        else => false,
    };
}

/// Rec. 601 perceived luminance. The cheap standard weighting, and enough
/// for the one question asked of it below.
fn luminance(color: canvas.Color) f32 {
    return 0.299 * color.r + 0.587 * color.g + 0.114 * color.b;
}

/// Whichever of the terminal's own `a` or `b` reads on `wash`.
fn readableOver(wash: canvas.Color, a: canvas.Color, b: canvas.Color) canvas.Color {
    const target = luminance(wash);
    return if (@abs(luminance(a) - target) >= @abs(luminance(b) - target)) a else b;
}

fn blend(a: canvas.Color, b: canvas.Color, t: f32) canvas.Color {
    return canvas.Color.rgba(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        1,
    );
}

fn scale(color: canvas.Color, factor: f32) canvas.Color {
    return canvas.Color.rgba(
        @min(1, color.r * factor),
        @min(1, color.g * factor),
        @min(1, color.b * factor),
        1,
    );
}
