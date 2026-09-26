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
const geist_register = canvas.DesignTokens.theme(.{ .color_scheme = .dark, .pack = .geist });

/// The durable location marker for selected tabs, sections, and navigation
/// rows. Wave 2 applies this to shipping markup because the SDK deliberately
/// leaves selected ghost buttons visually at rest unless they are toggles.
pub const selection_indicator = canvas.Color.rgb8(190, 242, 100);

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
    .accent = selection_indicator,
    .accent_text = canvas.Color.rgb8(9, 11, 15),
    .accent_hover = canvas.Color.rgb8(217, 249, 157),
    .accent_pressed = canvas.Color.rgb8(163, 230, 53),
    .focus = canvas.accentFocusRing(selection_indicator, .dark),
    .attention = canvas.Color.rgb8(253, 224, 71),
    .attention_text = canvas.Color.rgb8(9, 11, 15),
    .destructive = canvas.Color.rgb8(248, 113, 113),
};

pub const Geometry = struct {
    space_xs: f32,
    space_sm: f32,
    space_md: f32,
    space_lg: f32,
    space_xl: f32,
    control_sm: f32,
    control: f32,
    control_lg: f32,
    tab: f32,
    indicator: f32,
    hairline: f32,
    focus_stroke: f32,
    focus_offset: f32,
};

/// Cockpit names the Geist geometry it has intentionally adopted; the pinned
/// SDK remains the value authority. Unnamed metrics, including row extent,
/// pass through untouched.
pub const geometry: Geometry = .{
    .space_xs = geist_register.spacing.xs,
    .space_sm = geist_register.spacing.sm,
    .space_md = geist_register.spacing.md,
    .space_lg = geist_register.spacing.lg,
    .space_xl = geist_register.spacing.xl,
    .control_sm = geist_register.metrics.control_height_sm,
    .control = geist_register.metrics.control_height,
    .control_lg = geist_register.metrics.control_height_lg,
    .tab = geist_register.metrics.tabs_trigger_height,
    .indicator = geist_register.metrics.tabs_indicator_thickness,
    .hairline = geist_register.stroke.hairline,
    .focus_stroke = geist_register.stroke.focus,
    .focus_offset = geist_register.stroke.focus_offset,
};

pub const Radii = struct {
    control: f32,
    surface: f32,
};

pub const radii: Radii = .{
    .control = geist_register.radius.sm,
    .surface = geist_register.radius.xl,
};

/// Semantic recipes are optional-field overrides so applying them preserves
/// SDK-owned disabled colors and component-specific details from Geist.
pub const StateRecipes = struct {
    passive_surface: canvas.ControlVisualTokenOverrides,
    toolbar_action: canvas.ControlVisualTokenOverrides,
    quiet_action: canvas.ControlVisualTokenOverrides,
    default_action: canvas.ControlVisualTokenOverrides,
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
            // Secondary buttons couple pressed and selected through the
            // active channel. Shipping toolbar actions are not selection-
            // bearing, so spend that channel on visible pointer feedback.
            .active_background = palette.pressed,
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
        // The SDK's default variant shares active_background between pressed
        // and selected. Shipping CTAs are not selection-bearing, so this
        // dedicated recipe spends that coupled channel on pointer feedback
        // without changing explicit primary selection identity.
        .default_action = .{
            .background = palette.accent,
            .hover_background = palette.accent_hover,
            .active_background = palette.accent_pressed,
            .pressed_background = palette.accent_pressed,
            .foreground = palette.accent_text,
            .border = palette.accent,
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
            // Toggle buttons merge this table over their button variant.
            // State every visible rest channel so default CTA lime cannot
            // leak into inactive tab chrome through that fallback.
            .background = palette.surface,
            .hover_background = palette.hover,
            .active_background = palette.selected,
            .pressed_background = palette.pressed,
            .foreground = palette.text,
            .border = palette.border,
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
        .controls = .{
            .button_default = states.default_action,
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
    return renderedColor(kind, variant, state, .{}, part);
}

fn renderedColor(kind: canvas.WidgetKind, variant: canvas.WidgetVariant, state: canvas.WidgetState, style: canvas.WidgetStyle, part: u4) !canvas.Color {
    var commands: [16]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try canvas.emitWidgetTree(&builder, .{
        .id = test_widget_id,
        .kind = kind,
        .frame = native_sdk.geometry.RectF.init(0, 0, 160, 48),
        .text = "Semantic state",
        .variant = variant,
        .state = state,
        .style = style,
    }, designTokens());
    const command = builder.displayList().findCommandById(canvas.widgetPartId(test_widget_id, part)) orelse return error.TestExpectedFill;
    return switch (command.command) {
        .fill_rect => |fill| fillColor(fill.fill),
        .fill_rounded_rect => |fill| fillColor(fill.fill),
        .stroke_rect => |stroke| fillColor(stroke.stroke.fill),
        .draw_text => |text| text.color,
        else => error.TestExpectedFill,
    };
}

fn fillColor(fill: canvas.Fill) !canvas.Color {
    return switch (fill) {
        .color => |color| color,
        else => error.TestExpectedColor,
    };
}

pub fn contrastRatio(foreground: canvas.Color, background: canvas.Color) f32 {
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
    try std.testing.expect(!std.meta.eql(rest, pressed));
    try std.testing.expect(!std.meta.eql(hovered, pressed));
}

test "default buttons preserve the shipping CTA state ladder" {
    const rest = try renderedFill(.button, .default, .{}, 1);
    const hovered = try renderedFill(.button, .default, .{ .hovered = true }, 1);
    const pressed = try renderedFill(.button, .default, .{ .hovered = true, .pressed = true }, 1);

    try std.testing.expectEqual(palette.accent, rest);
    try std.testing.expectEqual(palette.accent_hover, hovered);
    try std.testing.expectEqual(palette.accent_pressed, pressed);
    try std.testing.expect(!std.meta.eql(rest, hovered));
    try std.testing.expect(!std.meta.eql(rest, pressed));
    try std.testing.expect(!std.meta.eql(hovered, pressed));
}

test "filled action selection keeps each SDK variant's active contract" {
    const default_selected = try renderedFill(.button, .default, .{ .selected = true }, 1);
    const primary_selected = try renderedFill(.button, .primary, .{ .selected = true }, 1);

    // Default couples selected and pressed through active_background.
    try std.testing.expectEqual(palette.accent_pressed, default_selected);
    // Primary has a dedicated pressed channel, so selected retains identity.
    try std.testing.expectEqual(palette.accent, primary_selected);
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

test "shipping ghost tabs keep safe distinct chrome through every current state" {
    const shipping_style: canvas.WidgetStyle = .{};
    const states = [_]canvas.WidgetState{
        .{},
        .{ .hovered = true },
        .{ .selected = true },
        .{ .hovered = true, .pressed = true },
    };
    const expected_fills = [_]canvas.Color{
        palette.surface,
        palette.hover,
        palette.selected,
        palette.pressed,
    };

    for (states, expected_fills) |state, expected_fill| {
        const fill = try renderedColor(.toggle_button, .ghost, state, shipping_style, 1);
        const text = try renderedColor(.toggle_button, .ghost, state, shipping_style, 4);
        try std.testing.expectEqual(expected_fill, fill);
        try std.testing.expectEqual(palette.text, text);
        try std.testing.expect(contrastRatio(text, fill) >= 4.5);
    }
    try std.testing.expect(!std.meta.eql(expected_fills[0], expected_fills[1]));
    try std.testing.expect(!std.meta.eql(expected_fills[0], expected_fills[2]));
    try std.testing.expect(!std.meta.eql(expected_fills[1], expected_fills[2]));
    try std.testing.expect(!std.meta.eql(expected_fills[0], expected_fills[3]));
    try std.testing.expect(!std.meta.eql(expected_fills[1], expected_fills[3]));
    try std.testing.expect(!std.meta.eql(expected_fills[2], expected_fills[3]));
}

test "secondary toolbar actions render distinct pointer feedback" {
    const rest = try renderedFill(.button, .secondary, .{}, 1);
    const hovered = try renderedFill(.button, .secondary, .{ .hovered = true }, 1);
    const pressed = try renderedFill(.button, .secondary, .{ .hovered = true, .pressed = true }, 1);

    try std.testing.expectEqual(palette.surface, rest);
    try std.testing.expectEqual(palette.hover, hovered);
    try std.testing.expectEqual(palette.pressed, pressed);
    try std.testing.expect(!std.meta.eql(rest, hovered));
    try std.testing.expect(!std.meta.eql(rest, pressed));
    try std.testing.expect(!std.meta.eql(hovered, pressed));
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
    try std.testing.expect(contrastRatio(palette.focus, palette.hover) >= 3.0);
    try std.testing.expect(contrastRatio(tokens.colors.warning_text, tokens.colors.warning) >= 4.5);
    try std.testing.expect(!std.meta.eql(tokens.colors.warning, tokens.colors.accent));
}

test "semantic geometry resolves from Geist without changing unadopted metrics" {
    const geist = canvas.DesignTokens.theme(.{ .color_scheme = .dark, .pack = .geist });
    const tokens = designTokens();
    try std.testing.expectEqual(geist.metrics.control_height, geist.metrics.control_height_sm + 2 * geist.spacing.xs);
    try std.testing.expect(std.meta.eql(geist.spacing, tokens.spacing));
    try std.testing.expect(std.meta.eql(geist.radius, tokens.radius));
    try std.testing.expect(std.meta.eql(geist.stroke, tokens.stroke));
    try std.testing.expect(std.meta.eql(geist.metrics, tokens.metrics));
    try std.testing.expectEqual(geist.spacing.xs, geometry.space_xs);
    try std.testing.expectEqual(geist.spacing.sm, geometry.space_sm);
    try std.testing.expectEqual(geist.spacing.md, geometry.space_md);
    try std.testing.expectEqual(geist.spacing.lg, geometry.space_lg);
    try std.testing.expectEqual(geist.spacing.xl, geometry.space_xl);
    try std.testing.expectEqual(geist.metrics.control_height_sm, geometry.control_sm);
    try std.testing.expectEqual(geist.metrics.control_height, geometry.control);
    try std.testing.expectEqual(geist.metrics.control_height_lg, geometry.control_lg);
    try std.testing.expectEqual(geist.metrics.tabs_trigger_height, geometry.tab);
    try std.testing.expectEqual(geist.metrics.tabs_indicator_thickness, geometry.indicator);
    try std.testing.expectEqual(geist.radius.sm, radii.control);
    try std.testing.expectEqual(geist.radius.xl, radii.surface);
    try std.testing.expectEqual(geist.stroke.hairline, geometry.hairline);
    try std.testing.expectEqual(geist.stroke.focus, geometry.focus_stroke);
    try std.testing.expectEqual(geist.stroke.focus_offset, geometry.focus_offset);
    try std.testing.expectEqual(geist.metrics.row_extent, tokens.metrics.row_extent);
    try std.testing.expectEqual(@as(f32, 28), tokens.metrics.row_extent);
}

test "selection indicator clears non-text contrast on row state surfaces" {
    try std.testing.expectEqual(palette.accent, selection_indicator);
    for ([_]canvas.Color{ palette.background, palette.surface, palette.hover, palette.selected }) |surface| {
        try std.testing.expect(contrastRatio(selection_indicator, surface) >= 3.0);
    }
}

test "Settings rescue text remains readable on every semantic surface" {
    const tokens = designTokens();
    for ([_]canvas.Color{ tokens.colors.background, tokens.colors.surface, tokens.colors.surface_subtle, tokens.colors.surface_pressed }) |surface| {
        try std.testing.expect(contrastRatio(tokens.colors.text, surface) >= 7.0);
        try std.testing.expect(contrastRatio(tokens.colors.text_muted, surface) >= 4.5);
    }
    try std.testing.expect(contrastRatio(tokens.colors.accent_text, tokens.colors.accent) >= 4.5);
}
