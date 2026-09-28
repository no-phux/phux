//! Window, canvas and font identities shared by the native engine and the
//! TypeScript adapter. Menus, shortcuts and the window scene live in app.zon.
const std = @import("std");
const native_sdk = @import("native_sdk");
const model_module = @import("../model.zig");

const canvas = native_sdk.canvas;
/// Font registrations do not depend on the app's Model/Msg, so any UiApp
/// instantiation names the same type the TypeScript adapter consumes.
const TerminalApp = native_sdk.UiApp(model_module.Model, union(enum) { none });

comptime {
    // The model restates the SDK's secondary-window budget; drift would drop windows.
    std.debug.assert(model_module.max_secondary_windows == TerminalApp.max_ui_windows);
}

pub const canvas_label = "phux-cockpit-canvas";
pub const app_name = "Phux Cockpit";
pub const webkit_parking_extent: f32 = 1;
pub const main_window_label = "main";

pub const max_windows: usize = model_module.max_windows;
pub const max_secondary_windows: usize = model_module.max_secondary_windows;

/// Window and canvas labels for windows 1..N (window 0 is `main`). Both are
/// addressed by string from the platform and automation, so they are tables.
pub const secondary_window_labels = [max_secondary_windows][]const u8{
    "phux-window-1",
    "phux-window-2",
    "phux-window-3",
    "phux-window-4",
};
pub const secondary_canvas_labels = [max_secondary_windows][]const u8{
    "phux-cockpit-canvas-1",
    "phux-cockpit-canvas-2",
    "phux-cockpit-canvas-3",
    "phux-cockpit-canvas-4",
};

/// The window index a canvas label names, or null for a label this app does
/// not own.
pub fn windowIndexForCanvas(label: []const u8) ?usize {
    if (std.mem.eql(u8, label, canvas_label)) return 0;
    for (secondary_canvas_labels, 0..) |candidate, offset| {
        if (std.mem.eql(u8, label, candidate)) return offset + 1;
    }
    return null;
}

pub fn windowLabelFor(index: usize) []const u8 {
    if (index == 0 or index > max_secondary_windows) return main_window_label;
    return secondary_window_labels[index - 1];
}

pub fn canvasLabelFor(index: usize) []const u8 {
    if (index == 0 or index > max_secondary_windows) return canvas_label;
    return secondary_canvas_labels[index - 1];
}

/// The terminal family, registered up front so SGR bold/italic use real
/// faces instead of synthesis.
pub const terminal_font_id: canvas.FontId = canvas.min_registered_font_id;
pub const terminal_bold_font_id: canvas.FontId = terminal_font_id + 1;
pub const terminal_italic_font_id: canvas.FontId = terminal_font_id + 2;
pub const terminal_bold_italic_font_id: canvas.FontId = terminal_font_id + 3;

pub const cockpit_fonts = [_]TerminalApp.FontRegistration{
    .{
        .id = terminal_font_id,
        .name = "JetBrainsMonoNL Nerd Font Mono Regular",
        .ttf = @embedFile("../../fonts/JetBrainsMonoNLNerdFontMono-Regular.ttf"),
    },
    .{
        .id = terminal_bold_font_id,
        .name = "JetBrainsMonoNL Nerd Font Mono Bold",
        .ttf = @embedFile("../../fonts/JetBrainsMonoNLNerdFontMono-Bold.ttf"),
    },
    .{
        .id = terminal_italic_font_id,
        .name = "JetBrainsMonoNL Nerd Font Mono Italic",
        .ttf = @embedFile("../../fonts/JetBrainsMonoNLNerdFontMono-Italic.ttf"),
    },
    .{
        .id = terminal_bold_italic_font_id,
        .name = "JetBrainsMonoNL Nerd Font Mono Bold Italic",
        .ttf = @embedFile("../../fonts/JetBrainsMonoNLNerdFontMono-BoldItalic.ttf"),
    },
};
