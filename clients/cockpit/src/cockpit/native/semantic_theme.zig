//! Cockpit's semantic presentation seam above the pinned SDK register.
//!
//! The SDK owns rendering mechanics and the Geist control ladder. This module
//! gives Cockpit's chrome one vocabulary for meaning: passive surfaces,
//! actions, tabs, navigation rows, settings fields, overlays, focus, and
//! attention. Feature views consume those meanings through `DesignTokens`;
//! they do not need to restate interaction colors or geometry.

const std = @import("std");
const native_sdk = @import("native_sdk");

const canvas = native_sdk.canvas;

pub const Palette = struct {
    background: canvas.Color,
    surface: canvas.Color,
    hover: canvas.Color,
    selected: canvas.Color,
    pressed: canvas.Color,
    text: canvas.Color,
    text_muted: canvas.Color,
    border: canvas.Color,
    accent: canvas.Color,
    accent_text: canvas.Color,
    accent_hover: canvas.Color,
    accent_pressed: canvas.Color,
    focus: canvas.Color,
    attention: canvas.Color,
    attention_text: canvas.Color,
    destructive: canvas.Color,
};

/// One dark graphite register. State washes remain neutral; lime is reserved
/// for location/focus identity and yellow for attention.
pub const palette: Palette = .{
    .background = canvas.Color.rgb8(9, 11, 15),
    .surface = canvas.Color.rgb8(17, 20, 27),
    .hover = canvas.Color.rgb8(23, 27, 35),
    .selected = canvas.Color.rgb8(35, 41, 52),
    .pressed = canvas.Color.rgb8(45, 53, 66),
    .text = canvas.Color.rgb8(244, 247, 251),
    .text_muted = canvas.Color.rgb8(154, 164, 178),
    .border = canvas.Color.rgb8(52, 58, 70),
    .accent = canvas.Color.rgb8(190, 242, 100),
    .accent_text = canvas.Color.rgb8(9, 11, 15),
    .accent_hover = canvas.Color.rgb8(217, 249, 157),
    .accent_pressed = canvas.Color.rgb8(163, 230, 53),
    .focus = canvas.accentFocusRing(canvas.Color.rgb8(190, 242, 100), .dark),
    .attention = canvas.Color.rgb8(253, 224, 71),
    .attention_text = canvas.Color.rgb8(9, 11, 15),
    .destructive = canvas.Color.rgb8(248, 113, 113),
};

pub const Geometry = struct {
    space_xs: f32 = 4,
    space_sm: f32 = 8,
    space_md: f32 = 12,
    space_lg: f32 = 16,
    space_xl: f32 = 24,
    control_sm: f32 = 32,
    control: f32 = 40,
    control_lg: f32 = 48,
    row: f32 = 32,
    tab: f32 = 50,
    indicator: f32 = 2,
    hairline: f32 = 1,
    focus_stroke: f32 = 2,
    focus_offset: f32 = 2,
};

pub const geometry: Geometry = .{};

pub const Radii = struct {
    control: f32 = 6,
    surface: f32 = 12,
};

pub const radii: Radii = .{};

/// Semantic recipes are optional-field overrides so applying them preserves
/// SDK-owned disabled colors and component-specific details from Geist.
pub const StateRecipes = struct {
    passive_surface: canvas.ControlVisualTokenOverrides,
    toolbar_action: canvas.ControlVisualTokenOverrides,
    quiet_action: canvas.ControlVisualTokenOverrides,
    primary_action: canvas.ControlVisualTokenOverrides,
    tab: canvas.ControlVisualTokenOverrides,
    navigation_row: canvas.ControlVisualTokenOverrides,
    settings_row: canvas.ControlVisualTokenOverrides,
    overlay_surface: canvas.ControlVisualTokenOverrides,
};

pub fn stateRecipes() StateRecipes {
    return .{
        // Hover deliberately equals rest. The pinned SDK treats passive
        // panels as pointer hit targets; this table keeps that routing useful
        // without making a non-action flash under the pointer. Press and
        // selection remain explicit if a surface is made actionable later.
        .passive_surface = .{
            .background = palette.surface,
            .hover_background = palette.surface,
            .active_background = palette.selected,
            .pressed_background = palette.pressed,
            .border = palette.border,
            .radius = radii.surface,
        },
        .toolbar_action = .{
            .background = palette.surface,
            .hover_background = palette.hover,
            .active_background = palette.selected,
            .pressed_background = palette.pressed,
            .foreground = palette.text,
            .border = palette.border,
            .radius = radii.control,
        },
        .quiet_action = .{
            .hover_background = palette.hover,
            .active_background = palette.selected,
            .pressed_background = palette.pressed,
            .foreground = palette.text,
            .radius = radii.control,
        },
        .primary_action = .{
            .background = palette.accent,
            .hover_background = palette.accent_hover,
            .active_background = palette.accent,
            .pressed_background = palette.accent_pressed,
            .foreground = palette.accent_text,
            .border = palette.accent,
            .radius = radii.control,
        },
        .tab = .{
            .hover_background = palette.hover,
            .active_background = palette.selected,
            .pressed_background = palette.pressed,
            .foreground = palette.text,
            .radius = radii.control,
        },
        .navigation_row = .{
            .hover_background = palette.hover,
            .active_background = palette.selected,
            .pressed_background = palette.pressed,
            .foreground = palette.text,
            .radius = radii.control,
        },
        .settings_row = .{
            .background = palette.surface,
            .hover_background = palette.hover,
            .active_background = palette.selected,
            .pressed_background = palette.pressed,
            .foreground = palette.text,
            .border = palette.border,
            .radius = radii.control,
        },
        .overlay_surface = .{
            .background = palette.surface,
            .hover_background = palette.surface,
            .active_background = palette.selected,
            .pressed_background = palette.pressed,
            .foreground = palette.text,
            .border = palette.border,
            .radius = radii.surface,
        },
    };
}

/// Complete app-owned chrome tokens. This is intentionally model-independent:
/// Cockpit exposes terminal appearance today, not a runtime chrome-theme axis.
pub fn designTokens() canvas.DesignTokens {
    var tokens = canvas.DesignTokens.theme(.{ .color_scheme = .dark, .pack = .geist });
    tokens = tokens.withOverrides(tokenOverrides());
    return tokens;
}

fn tokenOverrides() canvas.DesignTokenOverrides {
    const states = stateRecipes();
    return .{
        .colors = .{
            .background = palette.background,
            .surface = palette.surface,
            .surface_subtle = palette.hover,
            .surface_pressed = palette.selected,
            .text = palette.text,
            .text_muted = palette.text_muted,
            .border = palette.border,
            .accent = palette.accent,
            .accent_text = palette.accent_text,
            .focus_ring = palette.focus,
            .warning = palette.attention,
            .warning_text = palette.attention_text,
            .destructive = palette.destructive,
        },
        .spacing = .{
            .xs = geometry.space_xs,
            .sm = geometry.space_sm,
            .md = geometry.space_md,
            .lg = geometry.space_lg,
            .xl = geometry.space_xl,
        },
        .radius = .{
            .sm = radii.control,
            .md = radii.control,
            .lg = radii.control,
            .xl = radii.surface,
        },
        .stroke = .{
            .hairline = geometry.hairline,
            .regular = geometry.hairline,
            .focus = geometry.focus_stroke,
            .focus_offset = geometry.focus_offset,
        },
        .metrics = .{
            .control_height_sm = geometry.control_sm,
            .control_height = geometry.control,
            .control_height_lg = geometry.control_lg,
            .row_extent = geometry.row,
            .tabs_trigger_height = geometry.tab,
            .tabs_indicator_thickness = geometry.indicator,
        },
        .controls = .{
            .button_default = states.toolbar_action,
            .button_primary = states.primary_action,
            .button_secondary = states.toolbar_action,
            .button_outline = states.settings_row,
            .button_ghost = states.quiet_action,
            .toggle_button = states.tab,
            .select = states.settings_row,
            .input = states.settings_row,
            .text_field = states.settings_row,
            .search_field = states.settings_row,
            .combobox = states.settings_row,
            .textarea = states.settings_row,
            .list_item = states.navigation_row,
            .menu_item = states.navigation_row,
            .data_cell = states.navigation_row,
            .segmented_control = states.tab,
            .accordion = states.passive_surface,
            .alert = states.passive_surface,
            .bubble = states.passive_surface,
            .card = states.passive_surface,
            .panel = states.passive_surface,
            .resizable = states.passive_surface,
            .dialog = states.overlay_surface,
            .drawer = states.overlay_surface,
            .sheet = states.overlay_surface,
            .popover = states.overlay_surface,
            .menu_surface = states.overlay_surface,
            .dropdown_menu = states.overlay_surface,
            .tooltip = states.overlay_surface,
            .slider = .{ .active_background = palette.accent },
        },
    };
}

const test_widget_id: canvas.ObjectId = 9001;

fn renderedFill(kind: canvas.WidgetKind, variant: canvas.WidgetVariant, state: canvas.WidgetState, part: u4) !canvas.Color {
    var commands: [16]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try canvas.emitWidgetTree(&builder, .{
        .id = test_widget_id,
        .kind = kind,
        .frame = native_sdk.geometry.RectF.init(0, 0, 160, 48),
        .text = "Semantic state",
        .variant = variant,
        .state = state,
    }, designTokens());
    const command = builder.displayList().findCommandById(canvas.widgetPartId(test_widget_id, part)) orelse return error.TestExpectedFill;
    return switch (command.command) {
        .fill_rect => |fill| fillColor(fill.fill),
        .fill_rounded_rect => |fill| fillColor(fill.fill),
        .stroke_rect => |stroke| fillColor(stroke.stroke.fill),
        else => error.TestExpectedFill,
    };
}

fn fillColor(fill: canvas.Fill) !canvas.Color {
    return switch (fill) {
        .color => |color| color,
        else => error.TestExpectedColor,
    };
}

fn contrast(foreground: canvas.Color, background: canvas.Color) f32 {
    const foreground_luminance = luminance(foreground);
    const background_luminance = luminance(background);
    return (@max(foreground_luminance, background_luminance) + 0.05) /
        (@min(foreground_luminance, background_luminance) + 0.05);
}

fn luminance(color: canvas.Color) f32 {
    return 0.2126 * linearized(color.r) + 0.7152 * linearized(color.g) + 0.0722 * linearized(color.b);
}

fn linearized(channel: f32) f32 {
    if (channel <= 0.04045) return channel / 12.92;
    return std.math.pow(f32, (channel + 0.055) / 1.055, 2.4);
}

test "semantic state recipes render distinct actionable states" {
    const rest = try renderedFill(.button, .primary, .{}, 1);
    const hovered = try renderedFill(.button, .primary, .{ .hovered = true }, 1);
    const pressed = try renderedFill(.button, .primary, .{ .hovered = true, .pressed = true }, 1);

    try std.testing.expectEqual(palette.accent, rest);
    try std.testing.expectEqual(palette.accent_hover, hovered);
    try std.testing.expectEqual(palette.accent_pressed, pressed);
    try std.testing.expect(!std.meta.eql(rest, hovered));
    try std.testing.expect(!std.meta.eql(hovered, pressed));
}

test "navigation and tab selection share the semantic state ladder" {
    const row_hover = try renderedFill(.list_item, .default, .{ .hovered = true }, 1);
    const row_selected = try renderedFill(.list_item, .default, .{ .selected = true }, 1);
    const row_pressed = try renderedFill(.list_item, .default, .{ .hovered = true, .pressed = true }, 1);
    const tab_selected = try renderedFill(.toggle_button, .default, .{ .selected = true }, 1);

    try std.testing.expectEqual(palette.hover, row_hover);
    try std.testing.expectEqual(palette.selected, row_selected);
    try std.testing.expectEqual(palette.pressed, row_pressed);
    try std.testing.expectEqual(palette.selected, tab_selected);
    try std.testing.expect(!std.meta.eql(row_hover, row_selected));
    try std.testing.expect(!std.meta.eql(row_selected, row_pressed));
}

test "passive panel hover is visually stable without disabling hit testing" {
    const panel = canvas.Widget{
        .id = test_widget_id,
        .kind = .panel,
        .frame = native_sdk.geometry.RectF.init(0, 0, 160, 48),
    };
    try std.testing.expect(canvas.widgetIsHitTarget(panel));
    const rest = try renderedFill(.panel, .default, .{}, 2);
    const hovered = try renderedFill(.panel, .default, .{ .hovered = true }, 2);
    const pressed = try renderedFill(.panel, .default, .{ .pressed = true }, 2);

    try std.testing.expectEqual(palette.surface, rest);
    try std.testing.expectEqual(rest, hovered);
    try std.testing.expectEqual(palette.pressed, pressed);
}

test "focus and attention retain independent high-contrast signals" {
    const focus = try renderedFill(.button, .default, .{ .focused = true }, 3);
    const tokens = designTokens();

    try std.testing.expectEqual(palette.focus, focus);
    try std.testing.expect(contrast(palette.focus, palette.hover) >= 3.0);
    try std.testing.expect(contrast(tokens.colors.warning_text, tokens.colors.warning) >= 4.5);
    try std.testing.expect(!std.meta.eql(tokens.colors.warning, tokens.colors.accent));
}

test "semantic geometry remains on the Geist and four-point registers" {
    const tokens = designTokens();
    try std.testing.expectEqual(geometry.control, geometry.control_sm + 2 * geometry.space_xs);
    try std.testing.expectEqual(geometry.control_sm, tokens.metrics.control_height_sm);
    try std.testing.expectEqual(geometry.control, tokens.metrics.control_height);
    try std.testing.expectEqual(geometry.control_lg, tokens.metrics.control_height_lg);
    try std.testing.expectEqual(geometry.tab, tokens.metrics.tabs_trigger_height);
    try std.testing.expectEqual(geometry.indicator, tokens.metrics.tabs_indicator_thickness);
    try std.testing.expectEqual(radii.control, tokens.radius.sm);
    try std.testing.expectEqual(radii.surface, tokens.radius.xl);
    try std.testing.expectEqual(geometry.focus_stroke, tokens.stroke.focus);
    try std.testing.expectEqual(geometry.focus_offset, tokens.stroke.focus_offset);
}

test "Settings rescue text remains readable on every semantic surface" {
    const tokens = designTokens();
    for ([_]canvas.Color{ tokens.colors.background, tokens.colors.surface, tokens.colors.surface_subtle, tokens.colors.surface_pressed }) |surface| {
        try std.testing.expect(contrast(tokens.colors.text, surface) >= 7.0);
        try std.testing.expect(contrast(tokens.colors.text_muted, surface) >= 4.5);
    }
    try std.testing.expect(contrast(tokens.colors.accent_text, tokens.colors.accent) >= 4.5);
}
