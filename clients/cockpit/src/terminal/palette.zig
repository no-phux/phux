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

/// The emulator's color state, resolved for the painter.
///
/// The whole 256-entry palette comes from the EMULATOR, not the UI theme.
/// ANSI-16 used to be derived from `destructive`/`success`/`warning` design
/// tokens plus three hardcoded Tailwind hexes, which is why colored terminal
/// output looked wrong: `\x1b[31m` is a terminal red with a fixed, decades-old
/// meaning that `ls`, diff coloring, and every prompt theme are calibrated
/// against — it is not the UI's "something went wrong" accent. libghostty ships
/// the real defaults (`vt.color.default`, installed by `Terminal.init`), so the
/// projection reads them back verbatim. The cube, grayscale ramp, truecolor,
/// and OSC 4 overrides pass through the same single lookup.
///
/// Only foreground, background, and cursor stay theme-derived, and those the
/// EMULATOR composes: the session pushes tokens into `colors.*.default` so an
/// application's OSC 10/11/12 override still wins.
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
        // Two search washes, and they are two different HUES rather than two
        // intensities of one. A screen can hold dozens of matches, and the
        // question the eye is actually asking is "where am I" — a brightness
        // step alone does not answer that across a dense screen, while amber
        // (the match convention every editor and browser shares) against the
        // UI accent does. The match wash is pulled partway toward the
        // terminal's own background so ordinary matches stay readable text
        // rather than becoming a row of blocks.
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

    /// The color this cell's glyph is painted in, everything applied.
    ///
    /// `bg` is the cell's OWN background or null for "the terminal ground",
    /// and `cp` is the code point about to be drawn — the minimum-contrast
    /// floor needs both, which is why they are here rather than in
    /// `resolveFgRaw`.
    pub fn resolveFg(palette: *const Palette, style: vt.Style, bg: ?canvas.Color, cp: u21) canvas.Color {
        // The floor lands LAST, after inverse and faint, for the same reason
        // Ghostty applies it in the fragment stage rather than at attribute
        // resolution: it is a statement about the two colors that actually
        // meet on the glass. `\x1b[2;34m` resolves to a mid-blue that only
        // becomes illegible once the faint blend has happened, and an inverse
        // cell's ink is its background — neither is visible to a check run
        // before those steps.
        var color = if (style.flags.inverse) palette.resolveBgRaw(style) else fg: {
            var raw = palette.resolveFgRaw(style);
            if (style.flags.faint) raw = blend(raw, palette.background, 0.5);
            break :fg raw;
        };
        // Against `bg` — the cell's own resolved background, which is what the
        // painter fills behind this glyph — rather than against anything
        // recomputed here. `cellBackground` has arms this function does not
        // (the `bg_color_palette` / `bg_color_rgb` content tags), so a second
        // derivation would disagree with the paint on exactly the cells that
        // are hardest to notice.
        color = contrasted(palette.minimum_contrast, color, bg orelse palette.background, cp);
        return color;
    }

    /// `fg` raised until it clears `minimum_contrast` against `bg`, or `fg`
    /// unchanged when it already does.
    ///
    /// THIS IS GHOSTTY'S ALGORITHM, not a variation on it. See
    /// `src/renderer/shaders/shaders.metal`, `contrasted_color`: compute the
    /// WCAG 2.x ratio, and if it falls short replace the foreground OUTRIGHT
    /// with whichever of pure white or pure black scores higher against the
    /// background. Two properties of that choice are worth stating, because
    /// both look like bugs until you have the reason:
    ///
    ///   - The replacement is a SNAP, not a nudge. Walking the original hue
    ///     up until it just clears the floor sounds gentler and is worse: the
    ///     result is a color the application never asked for AND still barely
    ///     readable, and it makes the floor's effect depend on the hue, so
    ///     two colors that were equally illegible come out at different
    ///     brightnesses. White-or-black is the one answer that is always
    ///     maximally readable and always the same answer.
    ///   - It is chosen against the BACKGROUND alone, so a light theme gets
    ///     black ink and a dark theme gets white, with no theme awareness in
    ///     this function at all.
    ///
    /// Graphics code points are excluded, matching Ghostty's `noMinContrast`
    /// (`src/renderer/cell.zig`). Box drawing, block elements, Legacy
    /// Computing and Powerline glyphs are SHAPES painted in a foreground
    /// color: a Powerline separator is deliberately drawn in the color of the
    /// segment it divides, so "raise it until it contrasts" would repaint
    /// every prompt separator pure white and destroy the effect the glyph
    /// exists for.
    ///
    /// The luminance and ratio arithmetic is `config/theme.zig`'s, shared with
    /// the settings surface's legibility readout on purpose. Two copies of a
    /// WCAG curve is two things to keep in step, and the failure when they
    /// drift is a readout that tells the user their colours are fine while the
    /// renderer is quietly overriding them.
    pub fn contrasted(minimum_contrast: f32, fg: canvas.Color, bg: canvas.Color, cp: u21) canvas.Color {
        // Written as a negated `>` rather than `<= 1` so a NaN floor DISABLES
        // the check. The parser refuses a NaN today, but the failure mode if
        // one ever arrives is not symmetric: `ratio >= NaN` is false for every
        // ratio, so the wrong branch here repaints every cell in the terminal
        // pure white.
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
            // Bold-as-bright over the ANSI-8 range, the convention every
            // terminal ships and every prompt theme assumes: `\x1b[1;31m` is
            // bright red. Deliberately NOT applied to the bright range (8-15,
            // already bright), the cube, or truecolor, where the application
            // named an exact color.
            //
            // This SURVIVES `TerminalCell.bold` becoming a real projected
            // flag, and the two cannot double-apply because they act on
            // different channels: bold-as-bright is a COLOR decision resolved
            // here, `bold` is a WEIGHT request carried to the renderer. A
            // bold ANSI-1 cell therefore gets BOTH — the bright palette entry
            // and the flag. Dropping the brightening in favor of the flag
            // would be a visible regression today: the SDK draws one mono
            // face and synthesizes nothing, so `\x1b[1;31m` and `\x1b[31m`
            // would become pixel-identical. If a companion bold face ever
            // registers and the renderer starts embodying weight, the
            // brightening is the half to reconsider — it lives here, in one
            // switch arm, exactly so that stays a one-line decision.
            .palette => |index| palette.indexed(
                if (style.flags.bold and index < 8) index + 8 else index,
            ),
            .rgb => |rgb| canvas.Color.rgb8(rgb.r, rgb.g, rgb.b),
        };
    }

    /// SGR 58's underline color, or null when the cell never named one —
    /// which is the SDK's own default too (`TerminalCell.underline_color`
    /// null takes the cell foreground, the SGR 59 behavior). Indexed
    /// colors go through the same live 256-entry lookup as every other
    /// palette read, so an OSC 4 override moves the underline with the
    /// text it belongs to.
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

/// Code points the minimum-contrast floor must not touch.
///
/// A transcription of Ghostty's `noMinContrast` / `isGraphicsElement`
/// (`src/renderer/cell.zig`), ranges included, so the two builds exclude the
/// same set:
///
///   U+2500..U+257F   Box Drawing
///   U+2580..U+259F   Block Elements
///   U+1FB00..U+1FBFF Symbols for Legacy Computing
///   U+1CC00..U+1CEBF Symbols for Legacy Computing Supplement (Unicode 16.0)
///   U+E0B0..U+E0D7   Powerline, in the Private Use Area
///
/// These are not text. They are area fills and dividers whose foreground
/// colour is the DESIGN — a Powerline separator is drawn in the colour of the
/// segment behind it precisely so the two segments read as one shape, and a
/// shaded block is a brightness. Raising either to pure white does not make
/// anything more readable; it replaces the drawing with a white rectangle.
///
/// Ghostty's docs add that the floor also does not apply to emoji or images.
/// Emoji need no arm here: they carry their own colour, are never painted in
/// the cell foreground, and reach the glass through the SDK's colour-glyph
/// path rather than this projection's `fg`.
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

/// Whichever of `a` or `b` reads on `wash`. The candidates are the
/// TERMINAL's own foreground and background rather than black and white, so
/// a configured light theme keeps its palette instead of having a hardcoded
/// pair forced onto its search results.
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
