const std = @import("std");
const native_sdk = @import("native_sdk");
const model_module = @import("../model.zig");
const app_types = @import("../app_types.zig");

const canvas = native_sdk.canvas;
const TerminalApp = app_types.TerminalApp;

pub const canvas_label = "phux-cockpit-canvas";
pub const webview_label = "phux-cockpit-web";
pub const webview_anchor = "phux-cockpit-web-pane";
pub const app_name = "Phux Cockpit";
pub const bundle_id = "dev.phux.cockpit";
pub const window_width: f32 = 1100;
pub const window_height: f32 = 640;
pub const window_min_width: f32 = 900;
pub const window_min_height: f32 = 420;
pub const webkit_parking_extent: f32 = 1;

pub const web_origins = [_][]const u8{
    "zero://inline",
    "zero://app",
    "https://github.com",
    "https://www.superlogical.com",
    "https://mitchellh.com",
};

pub const cockpit_shortcuts = [_]native_sdk.Shortcut{
    .{ .id = "surface.1", .key = "1", .modifiers = .{ .primary = true } },
    .{ .id = "surface.2", .key = "2", .modifiers = .{ .primary = true } },
    .{ .id = "surface.3", .key = "3", .modifiers = .{ .primary = true } },
    .{ .id = "surface.4", .key = "4", .modifiers = .{ .primary = true } },
    .{ .id = "surface.5", .key = "5", .modifiers = .{ .primary = true } },
    .{ .id = "surface.web", .key = "b", .modifiers = .{ .primary = true, .shift = true } },
    // The summoned tab switcher. Registered globally like every other chord
    // the app answers, so the platform does not beep at it, and reachable
    // whatever surface owns the content area.
    .{ .id = "tabs.palette", .key = "p", .modifiers = .{ .primary = true, .shift = true } },
    // cmd+, — the settings chord every macOS app answers. Registered globally
    // like the rest so the platform does not beep at it, and so the menu item
    // below has a real equivalent rather than a decorative one.
    .{ .id = "settings.open", .key = ",", .modifiers = .{ .primary = true } },
    .{ .id = "tab.previous", .key = "[", .modifiers = .{ .primary = true, .shift = true } },
    .{ .id = "tab.next", .key = "]", .modifiers = .{ .primary = true, .shift = true } },
    .{ .id = "terminal.new", .key = "t", .modifiers = .{ .primary = true } },
    // cmd+N opens a WINDOW, the way every Mac app spells it — and the reason
    // cmd+T is the tab chord rather than this one.
    .{ .id = "window.new", .key = "n", .modifiers = .{ .primary = true } },
    .{ .id = "terminal.close", .key = "w", .modifiers = .{ .primary = true } },
    // The platform's own Enter Full Screen chord. Registering it is what stops
    // AppKit beeping at a key the app answers, and it addresses the FOCUSED
    // window rather than the main one.
    .{ .id = "window.fullscreen", .key = "f", .modifiers = .{ .primary = true, .control = true } },
    // cmd+M, for the same reason as fullscreen and with the same cause:
    // supplying ANY custom menu replaces the toolkit's stock bar, and the
    // stock bar is where Minimize lived. Without this the chord every Mac app
    // answers is one this app swallows.
    .{ .id = "window.minimize", .key = "m", .modifiers = .{ .primary = true } },
    // Splitting is unconditional now: one terminal on a fresh window is a
    // perfectly good thing to split, so the chord registers globally.
    .{ .id = "pane.split-right", .key = "d", .modifiers = .{ .primary = true } },
    .{ .id = "pane.split-down", .key = "d", .modifiers = .{ .primary = true, .shift = true } },
    .{ .id = "pane.previous", .key = "[", .modifiers = .{ .primary = true } },
    .{ .id = "pane.next", .key = "]", .modifiers = .{ .primary = true } },
    .{ .id = "tab.move-left", .key = "arrowleft", .modifiers = .{ .primary = true, .shift = true } },
    .{ .id = "tab.move-right", .key = "arrowright", .modifiers = .{ .primary = true, .shift = true } },
    .{ .id = "terminal.select-all", .key = "a", .modifiers = .{ .primary = true } },
    .{ .id = "terminal.copy", .key = "c", .modifiers = .{ .primary = true } },
    .{ .id = "terminal.paste", .key = "v", .modifiers = .{ .primary = true } },
    .{ .id = "terminal.clear", .key = "k", .modifiers = .{ .primary = true } },
    // Scrollback search: the cmd+F / cmd+G / cmd+shift+G trio every macOS
    // app shares. Registered globally so the platform does not beep at a
    // chord the app answers, and so the menu items below have equivalents.
    .{ .id = "terminal.find", .key = "f", .modifiers = .{ .primary = true } },
    .{ .id = "terminal.find-next", .key = "g", .modifiers = .{ .primary = true } },
    .{ .id = "terminal.find-previous", .key = "g", .modifiers = .{ .primary = true, .shift = true } },
    // `=` is the physical key; the platform reports the chord as cmd+`=`
    // whether or not shift is down, so ONE registration covers cmd+= and
    // cmd++.
    .{ .id = "view.font-larger", .key = "=", .modifiers = .{ .primary = true } },
    .{ .id = "view.font-smaller", .key = "-", .modifiers = .{ .primary = true } },
    .{ .id = "view.font-reset", .key = "0", .modifiers = .{ .primary = true } },
    .{ .id = "pane.focus-left", .key = "arrowleft", .modifiers = .{ .primary = true, .option = true } },
    .{ .id = "pane.focus-right", .key = "arrowright", .modifiers = .{ .primary = true, .option = true } },
    .{ .id = "pane.focus-up", .key = "arrowup", .modifiers = .{ .primary = true, .option = true } },
    .{ .id = "pane.focus-down", .key = "arrowdown", .modifiers = .{ .primary = true, .option = true } },
};

/// The application menu bar.
///
/// Supplying ANY menu replaces the toolkit's stock File/Edit/View/Window bar
/// wholesale (`appkit_host.m` rebuilds `mainMenu` as the app menu plus these),
/// so everything a terminal user reaches for has to be re-stated here — the
/// stock Edit menu's Copy/Paste went through the AppKit responder chain and
/// never reached a terminal pane anyway, which is exactly why three
/// `onCommand` entries had no way to fire.
///
/// Every item's `command` is a name the TypeScript core already answers, so the
/// menu adds a surface, never a second code path. The key equivalents mirror
/// the registered shortcuts on purpose: the model's shortcut latch keys on the
/// physical key, so one edge executes once regardless of which channel
/// delivered it.
const shell_menu_items = [_]native_sdk.MenuItem{
    .{ .label = "New Window", .command = "window.new", .key = "n", .modifiers = .{ .primary = true } },
    .{ .label = "New Tab", .command = "terminal.new", .key = "t", .modifiers = .{ .primary = true } },
    .{ .label = "Split Right", .command = "pane.split-right", .key = "d", .modifiers = .{ .primary = true } },
    .{ .label = "Split Down", .command = "pane.split-down", .key = "d", .modifiers = .{ .primary = true, .shift = true } },
    .{ .separator = true },
    .{ .label = "Close", .command = "terminal.close", .key = "w", .modifiers = .{ .primary = true } },
};

const edit_menu_items = [_]native_sdk.MenuItem{
    .{ .label = "Copy", .command = "terminal.copy", .key = "c", .modifiers = .{ .primary = true } },
    .{ .label = "Paste", .command = "terminal.paste", .key = "v", .modifiers = .{ .primary = true } },
    .{ .separator = true },
    .{ .label = "Select All", .command = "terminal.select-all", .key = "a", .modifiers = .{ .primary = true } },
    .{ .label = "Clear", .command = "terminal.clear", .key = "k", .modifiers = .{ .primary = true } },
    .{ .separator = true },
    .{ .label = "Find", .command = "terminal.find", .key = "f", .modifiers = .{ .primary = true } },
    .{ .label = "Find Next", .command = "terminal.find-next", .key = "g", .modifiers = .{ .primary = true } },
    .{ .label = "Find Previous", .command = "terminal.find-previous", .key = "g", .modifiers = .{ .primary = true, .shift = true } },
};

/// "Enter Full Screen" is back, and it works.
///
/// Supplying ANY custom menu replaces the whole bar, and every item in a
/// custom menu is built by `commandMenuItem:` — an `NSMenuItem` whose action
/// is always `menuCommandItemClicked:`, routed back through `on_command`. So
/// the item was declarable all along and the app had nothing to PERFORM with:
/// `PlatformServices` reported `WindowInfo.fullscreen` and exposed no verb
/// that entered or left it. `Effects.setWindowFullscreen` /
/// `toggleFullscreenWindow` are that verb, so the item names a command the app
/// can now answer, against the FOCUSED window rather than always the main one.
const view_menu_items = [_]native_sdk.MenuItem{
    // Settings lives in VIEW rather than in the app menu, where macOS
    // convention puts it, because the app menu is the toolkit's: supplying
    // custom menus replaces File/Edit/View/Window and `appkit_host.m` rebuilds
    // `mainMenu` as the STOCK app menu plus these, so there is no seam to add
    // an item to the app menu through. View is the next most honest home — it
    // is where font size and tab placement already live, and everything in
    // this panel is appearance. The cmd+, equivalent is the platform's
    // regardless of which menu carries it.
    .{ .label = "Settings…", .command = "settings.open", .key = ",", .modifiers = .{ .primary = true } },
    .{ .separator = true },
    .{ .label = "Enter Full Screen", .command = "window.fullscreen", .key = "f", .modifiers = .{ .primary = true, .control = true } },
    .{ .separator = true },
    .{ .label = "Increase Font Size", .command = "view.font-larger", .key = "=", .modifiers = .{ .primary = true } },
    .{ .label = "Decrease Font Size", .command = "view.font-smaller", .key = "-", .modifiers = .{ .primary = true } },
    .{ .label = "Reset Font Size", .command = "view.font-reset", .key = "0", .modifiers = .{ .primary = true } },
    .{ .separator = true },
    .{ .label = "Toggle Tab Placement", .command = "tabs.toggle-placement" },
    .{ .label = "Web Surface", .command = "surface.web", .key = "b", .modifiers = .{ .primary = true, .shift = true } },
};

const window_menu_items = [_]native_sdk.MenuItem{
    // Minimize is FIRST, where macOS puts it in every Window menu — this menu
    // replaces the stock one, so the convention has to be restated rather than
    // inherited.
    .{ .label = "Minimize", .command = "window.minimize", .key = "m", .modifiers = .{ .primary = true } },
    .{ .separator = true },
    .{ .label = "Go to Terminal…", .command = "tabs.palette", .key = "p", .modifiers = .{ .primary = true, .shift = true } },
    .{ .separator = true },
    .{ .label = "Previous Tab", .command = "tab.previous", .key = "[", .modifiers = .{ .primary = true, .shift = true } },
    .{ .label = "Next Tab", .command = "tab.next", .key = "]", .modifiers = .{ .primary = true, .shift = true } },
    .{ .separator = true },
    .{ .label = "Move Tab Left", .command = "tab.move-left", .key = "arrowleft", .modifiers = .{ .primary = true, .shift = true } },
    .{ .label = "Move Tab Right", .command = "tab.move-right", .key = "arrowright", .modifiers = .{ .primary = true, .shift = true } },
    .{ .separator = true },
    .{ .label = "Previous Pane", .command = "pane.previous", .key = "[", .modifiers = .{ .primary = true } },
    .{ .label = "Next Pane", .command = "pane.next", .key = "]", .modifiers = .{ .primary = true } },
};

pub const cockpit_menus = [_]native_sdk.Menu{
    .{ .title = "Shell", .items = &shell_menu_items },
    .{ .title = "Edit", .items = &edit_menu_items },
    .{ .title = "View", .items = &view_menu_items },
    .{ .title = "Window", .items = &window_menu_items },
};

/// The menu-bar extra's INSTALL-time shell: the parts macOS reads once, when
/// the `NSStatusItem` is created. Everything that moves — the title, the tone,
/// the list of terminals — is derived from the model on every rebuild by
/// the former native status-item projection.
///
/// It exists because a terminal is the app you are LEAST likely to have in
/// front of you: a long build, an agent working through a task, a server
/// waiting to fail all happen behind whatever you switched to. The strip's
/// attention dot is drawn on a window nobody is looking at. This is the same
/// state, on the one surface that is always visible.
///
/// `open_command` fires on every menu open. It exists so the derivation is
/// re-read at the moment the menu is presented rather than at the last
/// rebuild, which for a background app can be a while ago.
pub const cockpit_status_item: TerminalApp.StatusItemOptions = .{
    .title = status_item_prefix,
    .tooltip = app_name,
    .open_command = "tray.opened",
};

/// The two letters in the menu bar. Not the full name: a menu-bar extra pays
/// for its width in a strip everything else on the machine is also competing
/// for, and "Phux Cockpit 3" would be the widest thing up there.
pub const status_item_prefix = "PX";

// WebKit is deliberately absent from the startup scene. The host materializes
// its parked child view only after the first nonblank canvas present; creating
// WKWebView behind this surface used to hold the synchronous startup-frame
// flush behind WebKit process startup and consume the whole 150 ms budget.
const shell_views = [_]native_sdk.ShellView{
    .{ .label = canvas_label, .kind = .gpu_surface, .fill = true, .role = "Phux Cockpit canvas", .accessibility_label = "Phux Cockpit", .gpu_backend = .metal, .gpu_pixel_format = .bgra8_unorm, .gpu_present_mode = .timer, .gpu_alpha_mode = .@"opaque", .gpu_color_space = .srgb, .gpu_vsync = true },
};
/// The window's declared label. `fx.closeWindow` addresses a window by this
/// name.
///
/// Closing the last tab closes this window AND quits: `closeWindow` on its
/// own leaves a live process, because AppKit only terminates after the last
/// window when the delegate asks it to. Both effects are sent, in that order,
/// so the window tears down normally and the shutdown lifecycle that flushes
/// the workspace layout still runs.
pub const main_window_label = "main";

/// The whole window ceiling this toolkit affords.
///
/// A second window can only be a MODEL-DECLARED one (`UiApp.Options.
/// windows_fn` + `window_view`); the SDK budgets four of those on top of the
/// scene's own, so five is the ceiling and it is the toolkit's, not this
/// app's.
///
/// Painting one used to be the blocker: every cell this app draws comes out of
/// the chrome builder, and the runtime installed an app-owned chrome display
/// list for the MAIN canvas only — a declared second window rendered its tab
/// strip, its dividers, and empty terminal rectangles. `ChromeOptions.
/// build_window` closed that: `rebuildWindowSlot` now takes the same
/// `installChromeDisplayList` path with the slot's own canvas label, so a
/// secondary window's terminals paint. See `terminal_painter.paintWindowIndex`.
pub const max_windows: usize = model_module.max_windows;
pub const max_secondary_windows: usize = model_module.max_secondary_windows;

/// The declared identity of window 1..N. Window 0 is the scene's own `main`.
///
/// Two parallel tables rather than one derived name, because both sides are
/// LABELS the platform and the automation harness address by string: a
/// generated name would be a name no snapshot, no screenshot verb, and no
/// `fx.closeWindow` call site could be written against.
pub const secondary_window_labels = [max_secondary_windows][]const u8{
    "phux-window-1",
    "phux-window-2",
    "phux-window-3",
    "phux-window-4",
};

/// Each secondary window's own `gpu_surface` view label. Distinct from
/// `canvas_label` and from each other — the SDK routes input, automation
/// verbs, and per-window chrome by this string, so a collision would merge two
/// windows' surfaces.
pub const secondary_canvas_labels = [max_secondary_windows][]const u8{
    "phux-cockpit-canvas-1",
    "phux-cockpit-canvas-2",
    "phux-cockpit-canvas-3",
    "phux-cockpit-canvas-4",
};

/// The window index a canvas label names, or null for a label this app does
/// not own. THE discriminator: `build_window`, `window_view`, and the frame
/// pump all arrive holding a canvas label and nothing else.
pub fn windowIndexForCanvas(label: []const u8) ?usize {
    if (std.mem.eql(u8, label, canvas_label)) return 0;
    for (secondary_canvas_labels, 0..) |candidate, offset| {
        if (std.mem.eql(u8, label, candidate)) return offset + 1;
    }
    return null;
}

/// The window index a WINDOW label names. `window_view` is keyed by this one.
pub fn windowIndexForWindow(label: []const u8) ?usize {
    if (std.mem.eql(u8, label, main_window_label)) return 0;
    for (secondary_window_labels, 0..) |candidate, offset| {
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

const shell_windows = [_]native_sdk.ShellWindow{.{
    .label = main_window_label,
    .title = app_name,
    .width = window_width,
    .height = window_height,
    .min_width = window_min_width,
    .min_height = window_min_height,
    .restore_state = false,
    .titlebar = .hidden_inset_tall,
    .views = &shell_views,
}};
pub const shell_scene: native_sdk.ShellConfig = .{ .windows = &shell_windows };

pub const terminal_font_id: canvas.FontId = canvas.min_registered_font_id;
/// The weighted companions. SGR bold and italic reach every cell and every
/// renderer, but a glyph cannot change weight without a face to change to —
/// without these the SDK falls back to synthesis (double-strike and shear),
/// which is a fallback, not a typeface.
///
/// Deliberately the Nerd Font Mono variants matching the regular above, and
/// not the plain `JetBrainsMonoNL-*` weights that ship inside the SDK's own
/// font package. Those carry no patched glyphs, so a bold prompt would lose
/// its powerline separators and devicons while a regular one kept them — a
/// difference that shows up only in somebody's actual shell.
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
