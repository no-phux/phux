//! Supported terminal faces, selected before measurement and painting.
const canvas = @import("native_sdk").canvas;

/// `choice` is config.FontChoice ({ bundled, geist }). Keeping this seam
/// structural lets the renderer use it without depending on config parsing.
/// The app registers the bundled Nerd Font family at IDs 64..67 in scene.zig.
/// Geist Mono is SDK builtin 2 on BOTH the host and reference renderer. Its
/// byte-identical TTF also ships in assets/fonts: AppKit activates that tree
/// from the dev root or bundle Resources, while the reference embeds it.
/// Without the asset AppKit silently substitutes its system mono. Zero
/// companions request the SDK's shared synthesis policy; retaining IDs 65..67
/// here would incorrectly mix JetBrains bold/italic with Geist regular.
pub fn apply(tokens: *canvas.DesignTokens, choice: anytype) void {
    const first = canvas.min_registered_font_id;
    const ids: [4]canvas.FontId = switch (choice) {
        .bundled => .{ first, first + 1, first + 2, first + 3 },
        .geist => .{ canvas.default_mono_font_id, 0, 0, 0 },
    };
    tokens.typography.mono_font_id = ids[0];
    tokens.typography.mono_bold_font_id = ids[1];
    tokens.typography.mono_italic_font_id = ids[2];
    tokens.typography.mono_bold_italic_font_id = ids[3];
}
