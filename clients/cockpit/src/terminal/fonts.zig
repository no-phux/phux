//! Supported terminal faces, selected before measurement and painting.
const canvas = @import("native_sdk").canvas;

/// JetBrains retains IDs 64..67; Paper has real regular/bold at 68..69.
/// Zero italic companions request the SDK's shared synthesis, never a face
/// from another family. Geist remains builtin 2 on both renderers.
pub fn apply(tokens: *canvas.DesignTokens, choice: anytype) void {
    const first = canvas.min_registered_font_id;
    const ids: [4]canvas.FontId = switch (choice) {
        .paper => .{ first + 4, first + 5, 0, 0 },
        .bundled => .{ first, first + 1, first + 2, first + 3 },
        .geist => .{ canvas.default_mono_font_id, 0, 0, 0 },
    };
    tokens.typography.mono_font_id = ids[0];
    tokens.typography.mono_bold_font_id = ids[1];
    tokens.typography.mono_italic_font_id = ids[2];
    tokens.typography.mono_bold_italic_font_id = ids[3];
}
