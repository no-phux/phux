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

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const Model = model_module.Model;
const Pane = @import("../../providers/local/provider.zig").Pane;
const TerminalRef = @import("../phux_support.zig").TerminalRef;
const chrome_command_envelope = projection.chrome_command_envelope;

pub const window_ground_command_id: u64 = 0x0c01;

pub const pane_dim_command_id_base: u64 = 0x0c10;
/// The focused pane's accent edge: four hairlines, so exactly four ids.
/// `pane_dim_command_id_base` spans at most `layout.max_panes` (16) ids from
/// 0x0c10, so 0x0c30 clears it with room.
pub const pane_focus_command_id_base: u64 = 0x0c30;
/// The focus edge's thickness, in points.
pub const pane_focus_edge_thickness: f32 = 1;
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
/// Pane id namespaces are the REGISTRY slot, which is global — so two windows
/// showing different terminals never collide, and no window can ever show the
/// same terminal as another (`Model.admitTab` refuses it).
fn paintWindow(model: *const Model, builder: *canvas.Builder, window_index: usize, size: geometry.SizeF, tokens: canvas.DesignTokens) anyerror!void {
    const ws = model.wsAtConst(window_index) orelse return;
    // The grids paint with the TERMINAL tokens (the configured type size and
    // colors); everything else on this surface is chrome and keeps the app's
    // own register. See `projection.terminalTokens`.
    const grid_tokens = projection.terminalTokensFrom(tokens, model);
    // The window's own ground is painted ONCE, before any pane. The first
    // pane used to be handed the whole window as its background frame, so
    // the emulator's background (OSC 11 included) bled under the tab strip
    // and the titlebar. It takes the terminal's background so a configured
    // `background` reaches the gutter too, rather than leaving a frame of
    // the app's default graphite around a themed terminal.
    try builder.fillRect(.{
        .id = window_ground_command_id,
        .rect = geometry.RectF.init(0, 0, size.width, size.height),
        .fill = .{ .color = grid_tokens.colors.background },
    });

    // The app's own grounds are chrome, not grid: the panes' command
    // envelope is measured from HERE, so adding a background fill can never
    // shave commands off the last pane's share.
    const prologue = builder.len;

    var panes: [layout.max_panes]layout.Pane = undefined;
    const count = projection.resolvePanesIn(model, ws, size, &panes);
    if (count == 0) return;

    // The budgets are partitioned by kind, exactly as the two-pane painter
    // did, generalized to N panes:
    //   commands  — CUMULATIVE across the prefix, so pane i may spend up to
    //               its share of the running total and the LAST pane may
    //               spend the whole envelope;
    //   text/path — RESERVES, so a pane holds back the shares belonging to
    //               the panes that paint after it;
    //   glyphs    — per-paint local, so each pane takes an equal slice.
    const share_divisor: usize = @max(1, count);
    const text_share = (canvas.max_display_list_text_bytes - canvas.terminal_grid.widget_text_reserve) / share_divisor;
    const path_share = (canvas.max_chart_path_elements_per_frame - canvas.terminal_grid.widget_path_reserve) / share_divisor;
    // Cells are the budget that actually bounds a terminal now, and unlike
    // text and paths they have no widget floor to hold back: nothing but a
    // terminal grid emits into the packed cell store. So the WHOLE store is
    // divided among the panes and each one reserves the shares belonging to
    // the panes painting after it.
    //
    // Deliberately not `terminal_grid.widget_cell_reserve`. Despite the name
    // that constant is `max/2` — the SDK's even split for exactly TWO panes,
    // not a widget reserve. Using it as a floor would hold back half the
    // store at every pane count: a lone full-screen terminal would get 16384
    // cells and a 320x96 grid needs 30720, so the single-pane case — the
    // common one — would start silently truncating again.
    const cell_share = canvas.max_display_list_cells / share_divisor;
    const focus_node = if (ws.selectedTreeConst()) |current| current.focus else layout.none;

    for (panes[0..count], 0..) |pane, index| {
        if (pane.rect.width <= 0 or pane.rect.height <= 0) continue;
        const remaining = count - 1 - index;
        // The command budget is measured against the builder's RUNNING
        // length, so each pane is granted its own equal slice above whatever
        // the panes before it spent. A fixed cumulative ladder starved the
        // later panes whenever an earlier one filled its share.
        // A CUMULATIVE ladder measured from the chrome prologue: pane i may
        // spend up to its share of the running total, and the LAST pane may
        // spend the whole envelope. A per-pane slice would strand the tail
        // of the envelope unused whenever an early pane came in cheap.
        const command_budget = prologue + chrome_command_envelope * (index + 1) / share_divisor;
        const text_reserve = canvas.terminal_grid.widget_text_reserve + text_share * remaining;
        const path_reserve = canvas.terminal_grid.widget_path_reserve + path_share * remaining;
        const glyph_budget = canvas.terminal_grid.widget_glyph_budget / share_divisor;
        const cell_reserve = cell_share * remaining;
        // Each pane owns its OWN background frame. Nothing paints outside
        // the pane it belongs to.
        // A window that is not the one the user is in shows no focused pane:
        // two windows both drawing a solid cursor would both claim the
        // keyboard, and only one of them has it.
        const window_active = model.focused and window_index == model.active_window;
        const options_focused = window_active and pane.node == focus_node;
        if (model.provider.terminalConst(pane.terminal)) |terminal| {
            const preview_target = terminal.session.hoveredOsc8Target();
            const preview_reserve = linkPreviewCommandReserve(terminal.session);
            try grid.paint(terminal.session, builder, .{
                .frame = pane.rect,
                .background_frame = pane.rect,
                .tokens = grid_tokens,
                .running = terminal.phase == .live or terminal.phase == .starting,
                .focused = options_focused,
                .selecting = terminal.selecting,
                // Reserve only while a preview is actually pending. Quiet and
                // saturated grids retain the entire terminal envelope.
                .command_budget = command_budget -| preview_reserve,
                .text_reserve = text_reserve,
                .glyph_budget = glyph_budget,
                .path_reserve = path_reserve,
                .cell_reserve = cell_reserve,
                .minimum_contrast = model.config.minimum_contrast,
                .id_base = grid.paneIdBase(terminalPaintIndex(model, pane.terminal)),
            });
            if (preview_target) |target| try paintLinkTargetPreview(terminal, index, pane.rect, tokens, builder, target);
        } else {
            const remote = model.phuxConst() orelse continue;
            const presentation = remote.presentation(pane.terminal) orelse continue;
            try grid.paintTerminalGrid(presentation.grid, builder, .{
                .frame = pane.rect,
                .background_frame = pane.rect,
                .tokens = grid_tokens,
                .running = presentation.phase == .live,
                .focused = options_focused,
                .selecting = if (model.remoteUiConst(pane.terminal)) |state| state.selecting else false,
                .command_budget = command_budget,
                .text_reserve = text_reserve,
                .glyph_budget = glyph_budget,
                .path_reserve = path_reserve,
                .cell_reserve = cell_reserve,
                .id_base = grid.paneIdBase(terminalPaintIndex(model, pane.terminal)),
            });
        }

        // Ghostty dims the splits you are not in, and it is the right answer:
        // with per-pane headers gone, a solid-versus-hollow cursor was the
        // ONLY thing telling you where your keystrokes were going, and a
        // cursor is a few pixels on a screen full of text.
        //
        // One pane never dims — there is nothing to disambiguate — and
        // neither does a window that does not have key, where nothing is
        // focused at all.
        if (count > 1 and window_active and pane.node != focus_node) {
            try builder.fillRect(.{
                .id = pane_dim_command_id_base + index,
                .rect = pane.rect,
                .fill = .{ .color = dim_scrim },
            });
        }
    }

    // The focused pane's edge, painted AFTER every pane and every scrim so a
    // neighbour's dim can never lie on top of it.
    //
    // The scrim alone is not enough and never was. It dims toward black, and a
    // terminal configured black — or one an application put there with OSC 11 —
    // has nothing left to take away, which is exactly the setup a terminal user
    // is most likely to be running. The edge is the signal that survives it.
    if (count > 1 and model.focused and window_index == model.active_window) {
        for (panes[0..count]) |pane| {
            if (pane.node != focus_node) continue;
            if (pane.rect.width <= 0 or pane.rect.height <= 0) continue;
            const t = @min(pane_focus_edge_thickness, @min(pane.rect.width, pane.rect.height) / 2);
            const edges = [4]geometry.RectF{
                geometry.RectF.init(pane.rect.x, pane.rect.y, pane.rect.width, t),
                geometry.RectF.init(pane.rect.x, pane.rect.y + pane.rect.height - t, pane.rect.width, t),
                geometry.RectF.init(pane.rect.x, pane.rect.y, t, pane.rect.height),
                geometry.RectF.init(pane.rect.x + pane.rect.width - t, pane.rect.y, t, pane.rect.height),
            };
            for (edges, 0..) |edge, edge_index| {
                try builder.fillRect(.{
                    .id = pane_focus_command_id_base + edge_index,
                    .rect = edge,
                    .fill = .{ .color = tokens.colors.accent },
                });
            }
            break;
        }
    }
}

/// The unfocused-pane scrim: BLACK at low alpha, which reads as "further
/// away" rather than as a tint.
///
/// It used to be the window's own ground colour at 0.36, and that was a no-op
/// in the default configuration and in most others. The scrim is painted over
/// panes whose background is that same ground, and compositing a colour over
/// itself yields that colour at every alpha — so the dim drew literally
/// nothing, and the only thing distinguishing the focused split in a four-way
/// was a solid-versus-hollow cursor.
///
/// Dimming toward black instead makes it independent of what the pane beneath
/// happens to be: a configured `background`, a theme, or an application's
/// OSC 11 all darken. The one case it still cannot serve is a terminal that is
/// already black, which is why `paintWindow` also draws an accent edge.
///
/// The DEPTH is 0.15, and it is deliberately shallow. Ghostty's
/// `unfocused-split-opacity` defaults to 0.85 — the same 15% — and that is not
/// a coincidence of taste: an unfocused split is still a split you are READING,
/// and a shell prompt is already full of deliberately dim colours that a heavy
/// wash takes below legibility. The first fix for the no-op overshot to 0.42
/// and made unfocused panes genuinely hard to read, which traded one real
/// problem for another. The accent edge carries the signal; the scrim only has
/// to whisper.
const dim_scrim: canvas.Color = canvas.Color.rgba(0, 0, 0, 0.15);
