//! The remote equivalent of local Session.snapshot's per-paint defaults.
const canvas = @import("native_sdk").canvas;

pub fn sync(remote: anytype, tokens: canvas.DesignTokens, cursor_color: anytype) void {
    remote.setColorPolicy(.{
        .foreground = tokens.colors.text,
        .background = tokens.colors.background,
        .cursor_fallback = if (cursor_color) |color| canvas.Color.rgb8(color.r, color.g, color.b) else tokens.colors.accent,
        .selection_color = tokens.colors.accent,
    });
}
