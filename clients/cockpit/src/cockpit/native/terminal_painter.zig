//! Native terminal-cell painter retained beneath the shipping `.native` chrome.
//! Geometry comes exclusively from `workspace_projection.zig`; no widget chrome
//! or app-coordinator callbacks live here.
const std = @import("std");
const native_sdk = @import("native_sdk");
const grid = @import("../../terminal/grid.zig");
const url_module = @import("../../terminal/url.zig");
const provider_contract = @import("provider_contract");
const model_module = @import("../model.zig");
const layout = @import("../layout.zig");
const projection = @import("workspace_projection.zig");
const paint_budget = @import("paint_budget.zig");
const search_painter = @import("search_painter.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const Model = model_module.Model;
const Pane = @import("../../providers/local/provider.zig").Pane;
const TerminalRef = @import("../phux_support.zig").TerminalRef;

test {
    _ = paint_budget;
    _ = @import("../../tests/remote_theme_tests.zig");
    _ = @import("../../native_paint_owner_tests.zig");
}

pub const window_ground_command_id: u64 = 0x0c01;

pub const pane_dim_command_id_base: u64 = 0x0c10;
/// Rounded card fills: one id per pane, 0x0c20..0x0c2F.
pub const pane_card_command_id_base: u64 = 0x0c20;
/// The focused pane's floating accent ring: one id per pane, 0x0c30..0x0c3F.
/// `pane_dim_command_id_base` spans at most `layout.max_panes` (16) ids from
/// 0x0c10, so 0x0c30 clears it with room.
pub const pane_focus_command_id_base: u64 = 0x0c30;
/// Hairline card borders: one id per pane, 0x0c40..0x0c4F. Link-preview
/// ids start at 0x0c50.
pub const pane_border_command_id_base: u64 = 0x0c40;
/// Pane-local OSC 8 target preview commands.
pub const link_preview_ground_command_id_base: u64 = 0x0c50;
pub const link_preview_text_command_id_base: u64 = 0x0c70;
pub const link_preview_authority_command_id_base: u64 = 0x0c90;
const link_preview_command_count: usize = 3;

pub fn linkPreviewCommandReserve(session: *grid.Session) usize {
    const target = session.hoveredOsc8Target() orelse return 0;
    const identity = url_module.targetIdentity(target) orelse return 0;
    return if (identity.effective_authority != null) link_preview_command_count else 0;
}

pub fn terminalPaintIndex(model: *const Model, terminal_ref: TerminalRef) usize {
    if (provider_contract.isLocal(terminal_ref)) {
        return model.provider.slotIndex(terminal_ref) orelse 0;
    }
    return @intCast(0x0000_8000_0000_0000 | (terminal_ref.hash() & 0x0000_7fff_ffff_ffff));
}

fn paneMeasuredCells(model: *const Model, tree: *const layout.Tree, pane: layout.Pane) usize {
    if (model.provider.terminalConst(pane.terminal)) |terminal| {
        return @as(usize, terminal.session.cols()) * @as(usize, terminal.session.rows());
    }
    if (model.remotePaintPresentationIn(tree, pane.terminal)) |presentation| {
        return @as(usize, presentation.cols) * @as(usize, presentation.rows);
    }
    return paint_budget.full_cells;
}

fn authorityPreviewText(builder: *canvas.Builder, tokens: canvas.DesignTokens, authority: []const u8, width: f32) !?[]const u8 {
    var canonical_buf: [url_module.max_url_bytes]u8 = undefined;
    for (authority, 0..) |byte, index| canonical_buf[index] = std.ascii.toLower(byte);
    const canonical = canonical_buf[0..authority.len];
    const text_size = tokens.typography.label_size;
    if (canvas.measureTextWidthForFont(tokens.text_measure, tokens.typography.font_id, canonical, text_size) <= width) {
        return try builder.allocTextBytes(canonical);
    }
    const marker = "...";
    if (canvas.measureTextWidthForFont(tokens.text_measure, tokens.typography.font_id, marker ++ "x", text_size) > width) return null;

    var start = canonical.len;
    while (start > 0) {
        start -= 1;
        const candidate_width = canvas.measureTextWidthForFont(
            tokens.text_measure,
            tokens.typography.font_id,
            canonical[start..],
            text_size,
        );
        const marker_width = canvas.measureTextWidthForFont(tokens.text_measure, tokens.typography.font_id, marker, text_size);
        if (candidate_width + marker_width > width) {
            start += 1;
            break;
        }
    }
    var staged: [url_module.max_url_bytes]u8 = undefined;
    @memcpy(staged[0..marker.len], marker);
    @memcpy(staged[marker.len..][0 .. canonical.len - start], canonical[start..]);
    return try builder.allocTextBytes(staged[0 .. marker.len + canonical.len - start]);
}

fn paintLinkTargetPreview(
    pane: *const Pane,
    pane_index: usize,
    rect: geometry.RectF,
    tokens: canvas.DesignTokens,
    builder: *canvas.Builder,
    target: []const u8,
) !void {
    const identity = url_module.targetIdentity(target) orelse return;
    // Non-authority schemes are still announced, but a URL-shaped mismatch
    // remains conservative because there is no HTTP authority to isolate.
    const authority = identity.effective_authority orelse return;
    const inset = projection.chrome_band_inset;
    const height = projection.chrome_band_height;
    const frame = geometry.RectF.init(
        rect.x + inset,
        rect.y + @max(0, rect.height - height - inset),
        @max(0, rect.width - inset * 2),
        @min(height, rect.height),
    );
    if (frame.width <= 0 or frame.height <= 0) return;
    const text_width = @max(0, frame.width - inset * 2);
    const authority_text = try authorityPreviewText(builder, tokens, authority, text_width) orelse return;

    try builder.fillRect(.{
        .id = link_preview_ground_command_id_base + pane_index,
        .rect = frame,
        .fill = .{ .color = tokens.colors.surface },
    });
    const text_size = tokens.typography.label_size;
    const line_height = frame.height / 2;
    try builder.drawText(.{
        .id = link_preview_authority_command_id_base + pane_index,
        .font_id = tokens.typography.font_id,
        .size = text_size,
        .origin = geometry.PointF.init(
            frame.x + inset,
            frame.y + (line_height + text_size * 0.7) * 0.5,
        ),
        .color = tokens.colors.accent,
        .text = authority_text,
        .text_layout = .{
            .max_width = text_width,
            .line_height = line_height,
            .wrap = .none,
            .overflow = .clip,
            .measure = tokens.text_measure,
        },
    });
    try builder.drawText(.{
        .id = link_preview_text_command_id_base + pane_index,
        .font_id = tokens.typography.font_id,
        .size = text_size,
        .origin = geometry.PointF.init(
            frame.x + inset,
            frame.y + line_height + (line_height + text_size * 0.7) * 0.5,
        ),
        .color = tokens.colors.text,
        .text = target,
        .text_layout = .{
            .max_width = text_width,
            .line_height = line_height,
            .wrap = .none,
            .measure = tokens.text_measure,
        },
    });
    // Receipt is written only after every command needed to identify the
    // destination made it into the display list.
    pane.session.markOsc8PreviewRendered(target);
}

pub fn paintWindowIndex(model: *const Model, builder: *canvas.Builder, window_index: usize, size: geometry.SizeF, tokens: canvas.DesignTokens, _: native_sdk.platform.WindowId) anyerror!void {
    return paintWindow(model, builder, window_index, size, tokens);
}

/// The grids of ONE window, painted as a variable-length chrome prefix beneath
/// that window's widget tree: real text through the canvas primitives, damage
/// kept row-shaped by stable command ids, one id namespace per pane.
///
/// Pane id namespaces use local registry slots or remote terminal refs within
/// each window's display list. The selected tree also carries the attachment:
/// independent clients may publish the same remote ref in different windows.
fn paintWindow(model: *const Model, builder: *canvas.Builder, window_index: usize, size: geometry.SizeF, tokens: canvas.DesignTokens) anyerror!void {
    const ws = model.wsAtConst(window_index) orelse return;
    const window_active = model.focused and window_index == model.active_window;
    // The grids paint with the TERMINAL tokens (the configured type size and
    // colors); everything else on this surface is chrome and keeps the app's
    // own register. See `projection.terminalTokens`.
    const grid_tokens = projection.terminalTokensFrom(tokens, model);
    var panes: [layout.max_panes]layout.Pane = undefined;
    const count = projection.resolvePanesIn(model, ws, size, &panes);
    // Split cards own their opaque backgrounds. Leave their gutters and
    // rounded corners clear so the host material beneath the canvas shows
    // through. A lone pane stays full-bleed; non-markup callers retain their
    // full-window ground because they have no measured material boundary.
    if (ws.shipping_terminal_space == null or count < 2) {
        try builder.fillRect(.{
            .id = window_ground_command_id,
            .rect = ws.shipping_terminal_space orelse geometry.RectF.init(0, 0, size.width, size.height),
            .fill = .{ .color = grid_tokens.colors.background },
        });
    }
    try search_painter.paintWorkspace(model, builder, ws, size, tokens, window_active);

    if (count == 0) return;
    const tree = ws.selectedTreeConst() orelse return;
    try paintTerminalContents(model, builder, tree, panes[0..count], tokens, window_active, ws.surface_scale_factor);

    // Borders and the focus ring follow all panes and their dim scrims, so a
    // neighbouring pane cannot cover the active pane's focus indication.
    // The ring also remains visible when a black terminal cannot dim further.
    try paintPaneChrome(builder, panes[0..count], tree.focus, tokens, window_active);
}

fn paintTerminalContents(model: *const Model, builder: *canvas.Builder, tree: *const layout.Tree, panes: []const layout.Pane, tokens: canvas.DesignTokens, window_active: bool, scale_factor: f32) !void {
    // Ground and search controls are outside the panes' command envelope.
    const prologue = builder.len;
    const count = panes.len;
    const focus_node = tree.focus;
    const grid_tokens = projection.terminalTokensFrom(tokens, model);
    // When measured pane cells fit the store every visible pane paints full;
    // otherwise the active window's focused pane gets a full grid and the rest
    // share the leftover as a last-N crop (`max_rows / 4`).
    var focused_flags: [layout.max_panes]bool = @splat(false);
    var pane_cells: [layout.max_panes]usize = @splat(paint_budget.full_cells);
    for (panes, 0..) |pane, index| {
        focused_flags[index] = pane.node == focus_node;
        pane_cells[index] = paneMeasuredCells(model, tree, pane);
    }
    const budget_plan = paint_budget.plan(.{
        .window_active = window_active,
        .pane_count = count,
        .focused = focused_flags[0..count],
        .pane_cells = pane_cells[0..count],
    });

    for (panes, 0..) |pane, index| {
        if (pane.rect.width <= 0 or pane.rect.height <= 0) continue;
        const alloc = budget_plan.forPane(index, prologue);
        const grid_rect = projection.paneGridRect(pane.rect, count);
        // Each pane owns its background frame, and the grid sits inside a
        // constant inset so focus never moves a cell. Only the active window
        // shows a focused pane (one solid cursor).
        const options_focused = window_active and pane.node == focus_node;
        try paintCard(builder, pane, index, count, grid_tokens, tokens);
        const painted = try paintPane(model, tree, builder, pane, index, tokens, .{
            .frame = grid_rect,
            .background_frame = grid_rect,
            .tokens = grid_tokens,
            .scale_factor = scale_factor,
            .running = false,
            .focused = options_focused,
            .selecting = false,
            .command_budget = alloc.command_budget,
            .text_reserve = alloc.text_reserve,
            .glyph_budget = alloc.glyph_budget,
            .path_reserve = alloc.path_reserve,
            .cell_reserve = alloc.cell_reserve,
            .row_fit = .last_n,
            .row_cap = alloc.rowCap(),
            .minimum_contrast = model.config.minimum_contrast,
            .id_base = grid.paneIdBase(terminalPaintIndex(model, pane.terminal)),
        });
        if (!painted) continue;

        try paintDim(builder, pane, index, count, window_active, focus_node, tokens);
    }
}

/// Dim only background splits in the active window. A single pane never dims.
fn paintDim(
    builder: *canvas.Builder,
    pane: layout.Pane,
    index: usize,
    count: usize,
    window_active: bool,
    focus_node: layout.NodeId,
    tokens: canvas.DesignTokens,
) !void {
    if (count < 2 or !window_active or pane.node == focus_node) return;
    try builder.fillRoundedRect(.{
        .id = pane_dim_command_id_base + index,
        .rect = pane.rect,
        .radius = projection.paneCardRadius(tokens),
        .fill = .{ .color = dim_scrim },
    });
}

fn paintCard(
    builder: *canvas.Builder,
    pane: layout.Pane,
    index: usize,
    count: usize,
    grid_tokens: canvas.DesignTokens,
    tokens: canvas.DesignTokens,
) !void {
    if (count < 2) return;
    try builder.fillRoundedRect(.{
        .id = pane_card_command_id_base + index,
        .rect = pane.rect,
        .radius = projection.paneCardRadius(tokens),
        .fill = .{ .color = grid_tokens.colors.background },
    });
}

fn paintPane(model: *const Model, tree: *const layout.Tree, builder: *canvas.Builder, pane: layout.Pane, index: usize, tokens: canvas.DesignTokens, base: grid.PaintOptions) !bool {
    var options = base;
    if (model.provider.terminalConst(pane.terminal)) |terminal| {
        try paintLocalPane(terminal, builder, index, tokens, options);
    } else {
        const remote = model.phuxForTreeConst(tree) orelse return false;
        @import("remote_color_policy.zig").sync(remote, options.tokens, model.config.resolvedCursorColor());
        const presentation = model.remotePaintPresentationIn(tree, pane.terminal) orelse return false;
        options.running = presentation.phase == .live;
        options.selecting = if (model.remoteUiForOwnerConst(presentation.owner)) |state| state.selecting else false;
        try grid.paintTerminalGrid(presentation.grid, builder, options);
        recordRemoteCell(model, presentation.owner, options.tokens, options.scale_factor);
    }
    return true;
}

fn paintLocalPane(terminal: *const Pane, builder: *canvas.Builder, index: usize, tokens: canvas.DesignTokens, base: grid.PaintOptions) !void {
    var options = base;
    options.running = terminal.phase == .live or terminal.phase == .starting;
    options.selecting = terminal.selecting;
    options.command_budget -|= linkPreviewCommandReserve(terminal.session);
    const preview_target = terminal.session.hoveredOsc8Target();
    try grid.paint(terminal.session, builder, options);
    if (preview_target) |target| try paintLinkTargetPreview(terminal, index, base.frame, tokens, builder, target);
}

fn paintPaneChrome(
    builder: *canvas.Builder,
    panes: []const layout.Pane,
    focus_node: layout.NodeId,
    tokens: canvas.DesignTokens,
    window_active: bool,
) !void {
    if (panes.len < 2 or !window_active) return;
    const radius = projection.paneCardRadius(tokens);
    const hairline = @max(1, tokens.stroke.hairline);
    // These borders sit on opaque terminal content, not native material. Keep
    // the terminal's opaque divider: light chrome's black/8% hairline would
    // disappear over a dark terminal when the system appearance changes.
    const border = projection.baseTokens().colors.border;
    for (panes, 0..) |pane, index| {
        if (pane.rect.width <= 0 or pane.rect.height <= 0) continue;
        try builder.strokeRect(.{
            .id = pane_border_command_id_base + index,
            .rect = pane.rect,
            .radius = radius,
            .stroke = .{ .fill = .{ .color = border }, .width = hairline },
        });
        if (pane.node != focus_node) continue;
        try builder.strokeRect(.{
            .id = pane_focus_command_id_base + index,
            .rect = projection.paneFocusRingRect(pane.rect, tokens),
            .radius = projection.paneFocusRingRadius(tokens),
            .stroke = .{ .fill = .{ .color = tokens.colors.accent }, .width = @max(1, tokens.stroke.focus) },
        });
    }
}

/// The unfocused-pane scrim: black at 15% (Ghostty's unfocused-split-opacity
/// depth), independent of the pane's own background; the accent ring carries
/// the focus signal.
const dim_scrim: canvas.Color = canvas.Color.rgba(0, 0, 0, 0.15);

fn recordRemoteCell(model: *const Model, owner: provider_contract.ReplicaOwner, tokens: canvas.DesignTokens, scale_factor: f32) void {
    if (comptime !@import("../phux_support.zig").phux_enabled) return;
    const remote = model.phuxForOwnerConst(owner) orelse return;
    const metrics = canvas.terminalCellMetrics(tokens);
    remote.host.recordMeasuredCell(owner, .{
        .width = metrics.width,
        .height = metrics.height,
        .font_id = tokens.typography.mono_font_id,
        .font_size = metrics.font_size,
        .scale_factor = scale_factor,
    });
}
