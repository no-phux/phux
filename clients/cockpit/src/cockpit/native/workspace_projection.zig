const std = @import("std");
const native_sdk = @import("native_sdk");
const provider_contract = @import("provider_contract");
const grid = @import("../../terminal/grid.zig");
const support = @import("../phux_support.zig");
const local = @import("../../providers/local/provider.zig");
const model_module = @import("../model.zig");
const topology = @import("../topology.zig");
const layout = @import("../layout.zig");
const scene = @import("scene.zig");
const config_module = @import("../../config/config.zig");
const theme_module = @import("../../config/theme.zig");
const fonts = @import("../../terminal/fonts.zig");
pub const semantic_theme = @import("semantic_theme.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const Model = model_module.Model;
const Workspace = model_module.Workspace;
const Pane = local.Pane;
const TerminalRef = support.TerminalRef;

/// The configured window gutter.
pub fn windowPadding(model: *const Model) f32 {
    return @max(0, model.config.window_padding);
}

/// The chrome register: a band is one default-register control tall (40) and
/// hosts small-register controls (32) with `spacing.xs` shoulders. See
/// docs/DESIGN_SYSTEM.md.
pub const chrome_band_height: f32 = semantic_theme.geometry.control;
pub const chrome_band_inset: f32 = semantic_theme.geometry.space_xs;
pub const chrome_control_extent: f32 = semantic_theme.geometry.control_sm;
pub const chrome_gap: f32 = semantic_theme.geometry.space_sm;

/// The tab band's fallback height; at least the register's trigger height so
/// the strip never paints into the terminal's first row.
pub const header_height: f32 = semantic_theme.geometry.tab;
pub const tab_extent: f32 = 168;
/// How far tabs shrink before the strip windows: 72pt of furniture plus a
/// readable label.
pub const tab_min_extent: f32 = 120;
pub const tab_height: f32 = chrome_band_height;

/// Clearance for AppKit's traffic lights (at 20/40/60pt) at the leading edge
/// of the titlebar band.
pub const titlebar_tab_leading_reserve: f32 = 78;

/// The shortest titlebar band that can host the tab strip.
pub const titlebar_tab_band_min: f32 = tab_height + 8;

/// Whether this window's titlebar band hosts the tab strip, so revealing tabs
/// costs no content height. Fullscreen has no titlebar and falls back to a
/// separate `header_height` band.
pub fn tabsRideTitlebarIn(model: *const Model, workspace: *const Workspace) bool {
    if (model.tab_placement != .top) return false;
    return titlebarBandHeight(model, workspace) >= titlebar_tab_band_min;
}

/// The titlebar band's own height, inside the window's padding.
fn titlebarBandHeight(model: *const Model, workspace: *const Workspace) f32 {
    const inset = windowPadding(model);
    return @max(0, @max(inset, workspace.chrome_top + 4) - inset);
}

pub const TabWindow = struct {
    first: usize = 0,
    count: usize = 0,
    /// How many tabs the workspace has, for edge cues.
    total: usize = 0,
    /// The width each visible tab lays out at, between `tab_min_extent` and
    /// `tab_extent`.
    extent: f32 = tab_extent,

    pub fn contains(window: TabWindow, index: usize) bool {
        return index >= window.first and index < window.first + window.count;
    }

    /// Whether this run is a WINDOW onto a longer list rather than the list.
    pub fn windowed(window: TabWindow) bool {
        return window.count < window.total;
    }
};

fn limitNoticeVisible(model: *const Model, workspace: *const Workspace) bool {
    return model.window_limit_refused or workspace.tab_limit_refused or model.terminal_limit_refused;
}

fn saveFailureNoticeVisible(model: *const Model) bool {
    return model.config_write_refused or model.state.write_failed;
}

pub const side_rail_width: f32 = 184;
pub const side_rail_gap: f32 = chrome_gap;
/// The grab band between two panes; even, so its hairline centres on a point.
pub const split_divider_width: f32 = chrome_gap;
pub const split_pane_min_width: f32 = 240;
pub const split_pane_min_height: f32 = 80;
/// Keeps the grid clear of a split pane card's rounded corners; a single
/// pane stays full-bleed.
pub const pane_chrome_inset: f32 = chrome_band_inset;

/// The rect the grid, the PTY and pointer cell-mapping all share.
pub fn paneGridRect(card: geometry.RectF, pane_count: usize) geometry.RectF {
    if (pane_count < 2) return card;
    return card.inset(geometry.InsetsF.all(pane_chrome_inset));
}

pub fn paneCardRadius(tokens: canvas.DesignTokens) canvas.Radius {
    return canvas.Radius.all(tokens.radius.md);
}

/// The focus ring floats `stroke.focus_offset` outside the card, so focus
/// never resizes the grid.
pub fn paneFocusRingRect(card: geometry.RectF, tokens: canvas.DesignTokens) geometry.RectF {
    return card.normalized().inflate(geometry.InsetsF.all(@max(0, tokens.stroke.focus_offset)));
}

pub fn paneFocusRingRadius(tokens: canvas.DesignTokens) canvas.Radius {
    const offset = @max(0, tokens.stroke.focus_offset);
    return canvas.Radius.all(tokens.radius.md + offset);
}
pub const webkit_parking_extent = scene.webkit_parking_extent;
const widget_command_reserve: usize = canvas.terminal_grid.widget_command_reserve;
pub const chrome_command_envelope: usize = native_sdk.runtime.max_canvas_commands_per_view - widget_command_reserve;

pub fn baseTokens() canvas.DesignTokens {
    var tokens = semantic_theme.designTokens();
    tokens.typography.mono_font_id = scene.terminal_font_id;
    // Real companion faces instead of synthesized bold/italic.
    tokens.typography.mono_bold_font_id = scene.terminal_bold_font_id;
    tokens.typography.mono_italic_font_id = scene.terminal_italic_font_id;
    tokens.typography.mono_bold_italic_font_id = scene.terminal_bold_italic_font_id;
    return tokens;
}

pub fn cockpitTokens(_: *const Model) canvas.DesignTokens {
    return baseTokens();
}

/// The tokens terminal grids paint with, separate from the chrome's so
/// `font-size` (which drives `label_size`) does not grow the chrome.
pub fn terminalTokens(model: *const Model) canvas.DesignTokens {
    return terminalTokensFrom(cockpitTokens(model), model);
}

/// The same derivation over an already-resolved token set. Taking a base is
/// load-bearing: it carries the runtime's text-measure provider, without which
/// cell metrics fall back to an estimate (see `terminalCellMetricsFor`).
pub fn terminalTokensFrom(base: canvas.DesignTokens, model: *const Model) canvas.DesignTokens {
    var tokens = base;
    const cfg = &model.config;
    // The base's colours follow chrome appearance; terminals keep their own
    // defaults unless terminal config says otherwise.
    const defaults = baseTokens().colors;
    tokens.colors.background = defaults.background;
    tokens.colors.text = defaults.text;
    tokens.colors.accent = defaults.accent;
    fonts.apply(&tokens, config_module.fontChoice(cfg.font_family.slice()) orelse .bundled);
    tokens.typography.label_size = model.fontSize();
    // These become emulator defaults (OSC 10/11 still win). Theme-vs-explicit
    // precedence lives in `Config.resolved*`.
    if (cfg.resolvedBackground()) |color| tokens.colors.background = canvas.Color.rgb8(color.r, color.g, color.b);
    if (cfg.resolvedForeground()) |color| tokens.colors.text = canvas.Color.rgb8(color.r, color.g, color.b);
    // The selection wash reads `colors.accent`; the cursor colour is applied
    // separately by `applySessionConfig`.
    if (cfg.resolvedSelectionBackground()) |color| tokens.colors.accent = canvas.Color.rgb8(color.r, color.g, color.b);
    return tokens;
}

/// The terminal's painted text and ground colours and their contrast, read
/// from the same tokens the painter uses.
pub const Legibility = struct {
    foreground: canvas.Color,
    background: canvas.Color,
    /// WCAG 2.x contrast ratio, 1.0 (identical) through 21.0 (black on white).
    ratio: f32,
    grade: theme_module.Legibility,

    pub fn readable(self: Legibility) bool {
        return self.grade.readable();
    }
};

/// `canvas.terminalCellMetrics` for tokens without a text-measure provider
/// (everywhere outside the painter). The SDK estimator only knows the default
/// mono id is monospace and would price our registered face as proportional
/// text (~46% too wide); our face is a 0.6 em mono, so estimate with the
/// default mono id instead.
pub fn terminalCellMetricsFor(tokens: canvas.DesignTokens) canvas.TerminalCellMetrics {
    if (tokens.text_measure != null) return canvas.terminalCellMetrics(tokens);
    var estimating = tokens;
    estimating.typography.mono_font_id = canvas.default_mono_font_id;
    return canvas.terminalCellMetrics(estimating);
}

/// The tab trigger height the band must contain.
pub fn tabTriggerHeight(model: *const Model) f32 {
    return cockpitTokens(model).metrics.tabs_trigger_height;
}

fn paneNeedsAttention(model: *const Model, pane: *const Pane) bool {
    const paste_failed = model.paste_owner.terminal_ref.eql(pane.id) and model.paste_failed;
    // Unacknowledged loss, not the cumulative count, or one dropped byte would
    // pin attention forever.
    return pane.bellRung() or
        pane.phase == .ended or pane.phase == .failed or
        pane.hasUnacknowledgedLoss() or pane.copy_failed or paste_failed;
}

/// Title ceiling, so a runaway OSC 0 cannot push a menu row off screen.
pub const max_terminal_title_bytes: usize = 96;

/// A terminal's 1-based tab position and pane (leaf order) within it.
pub const TerminalAddress = struct {
    tab: usize,
    pane: usize,
    /// One pane means the name drops the pane digit.
    pane_count: usize,
};

/// The address a terminal answers to, or null when it sits in no tab. By
/// position, not registry slot: slots only move forward, so a slot-numbered
/// name would disagree with the cmd+N that selects the tab.
pub fn terminalAddress(model: *const Model, id: TerminalRef) ?TerminalAddress {
    const where = model.locateTerminal(id) orelse return null;
    const workspace = model.wsAtConst(where.window) orelse return null;
    const tree = workspace.treeConst(where.tab) orelse return null;
    var refs: [layout.max_panes]TerminalRef = undefined;
    const count = tree.terminals(&refs);
    for (refs[0..count], 0..) |ref, index| {
        if (!ref.eql(id)) continue;
        return .{ .tab = where.tab + 1, .pane = index + 1, .pane_count = count };
    }
    return null;
}

/// A terminal's name: its OSC 0/2 title, else its OSC 7 directory basename,
/// else `Terminal N` / `Terminal N.M` (its address). `out` is used only for
/// the last case. Phux terminals use the coordinator's published title.
pub fn terminalTitleInto(model: *const Model, id: TerminalRef, out: []u8) []const u8 {
    if (provider_contract.isLocal(id)) {
        if (model.provider.terminalConst(id)) |pane| {
            const shell_title = pane.title();
            if (shell_title.len > 0) return clampTitle(shell_title);
            const cwd = pane.pwd();
            if (cwd.len > 0) {
                const leaf = std.fs.path.basename(cwd);
                if (leaf.len > 0) return clampTitle(leaf);
            }
        }
        const at = terminalAddress(model, id) orelse return "Terminal";
        if (at.pane_count <= 1) return std.fmt.bufPrint(out, "Terminal {d}", .{at.tab}) catch "Terminal";
        return std.fmt.bufPrint(out, "Terminal {d}.{d}", .{ at.tab, at.pane }) catch "Terminal";
    }
    return remoteTitle(model, id);
}

/// The same chain for a Phux terminal, reading what the replica reports live
/// (title, then the working-directory basename) before what the attach
/// catalog recorded, which can only be older.
fn remoteTitle(model: *const Model, id: TerminalRef) []const u8 {
    const presentation = model.remotePresentation(id);
    if (presentation) |value| {
        if (value.title.len != 0) return clampTitle(value.title);
        if (cwdLeaf(value.cwd)) |leaf| return clampTitle(leaf);
    }
    const entry = catalogEntry(model, id) orelse return "Phux";
    if (entry.title.len != 0) return clampTitle(entry.title.slice());
    if (cwdLeaf(entry.cwd.slice())) |leaf| return clampTitle(leaf);
    return "Phux";
}

fn catalogEntry(model: *const Model, id: TerminalRef) ?*const provider_contract.workspace.CatalogTerminal {
    const remote = model.phuxForRefConst(id) orelse return null;
    for (remote.catalogTerminals()) |*entry| {
        if (entry.terminal_ref.eql(id)) return entry;
    }
    return null;
}

/// The name a working directory gives a tab: its basename, or `/` for the
/// root (whose basename is empty). An empty directory names nothing.
fn cwdLeaf(cwd: []const u8) ?[]const u8 {
    const leaf = std.fs.path.basename(cwd);
    if (leaf.len != 0) return leaf;
    return if (cwd.len != 0) "/" else null;
}

/// Whether a terminal sits at a shell prompt, answered the same way for both
/// providers: the local emulator's OSC-133 read, or a Phux terminal's last
/// command boundary. False means "busy" or "unknown"; neither guesses.
pub fn terminalAtPrompt(model: *const Model, id: TerminalRef) bool {
    if (model.provider.terminalConst(id)) |pane| return pane.atPrompt();
    const remote = model.phuxForRefConst(id) orelse return false;
    return remote.atPrompt(id);
}

/// The coordinator whose shared workspace a tab came from.
fn tabCoordinator(model: *const Model, workspace: *const Workspace, index: usize) ?*const support.PhuxProvider {
    const tree = workspace.treeConst(index) orelse return null;
    const owner = @import("../shared_workspace.zig").tabAuthority(tree) orelse return null;
    return model.phuxForConst(owner);
}

pub fn tabTitleInto(model: *const Model, workspace: *const Workspace, index: usize, out: []u8) []const u8 {
    if (workspace.shared_ids[index]) |id| {
        if (tabCoordinator(model, workspace, index)) |remote| {
            for (remote.workspaceSnapshot().windows) |*window| {
                if (!std.mem.eql(u8, &window.id, &id)) continue;
                if (window.name.len != 0) return clampTitle(window.name.slice());
                break;
            }
        }
    }
    const ref = workspace.tabTerminal(index) orelse return "Terminal";
    return terminalTitleInto(model, ref, out);
}

/// Cut an over-long title at a UTF-8 boundary.
fn clampTitle(title: []const u8) []const u8 {
    if (title.len <= max_terminal_title_bytes) return title;
    var end = max_terminal_title_bytes;
    while (end > 0 and title[end] & 0xc0 == 0x80) end -= 1;
    return title[0..end];
}

/// Whether the strip can still tell its tabs apart: painted labels
/// (`distinct`) versus the full names behind them (`nameable`).
pub const TabLabelIdentity = struct {
    /// Tabs the strip is showing.
    shown: usize = 0,
    /// Distinct FULL names among them.
    nameable: usize = 0,
    /// Distinct PAINTED labels among them.
    distinct: usize = 0,
    /// The label box every visible tab was given.
    label_width: f32 = 0,

    /// The strip painted away identity its tabs' own names carried.
    pub fn ambiguous(self: TabLabelIdentity) bool {
        return self.distinct < self.nameable;
    }
};

/// A tab summarizes all of its splits, including a quiet focused leaf's
/// blocked sibling. Use the same terminal predicate for every placement.
pub fn tabNeedsAttention(model: *const Model, workspace: *const Workspace, index: usize) bool {
    const tree = workspace.treeConst(index) orelse return false;
    var refs: [layout.max_panes]TerminalRef = undefined;
    const count = tree.terminals(&refs);
    for (refs[0..count]) |ref| {
        if (terminalNeedsAttention(model, ref)) return true;
    }
    return false;
}

pub fn terminalNeedsAttention(model: *const Model, id: TerminalRef) bool {
    if (model.provider.terminalConst(id)) |pane| return paneNeedsAttention(model, pane);
    // Stream-derived attention sits beside the bell rather than replacing it.
    // A terminal is worth a marker if it rang, if its replica is in trouble,
    // OR if an agent running under it is waiting on an answer.
    if (model.agentAttention(id)) return true;
    if (comptime support.phux_enabled) {
        if (model.phuxForRefConst(id)) |remote| if (remote.bellRung(id)) return true;
    }
    const presentation = model.remotePresentation(id) orelse return true;
    return presentation.phase == .failed or presentation.phase == .tombstoned;
}

/// One agent session as a row under its terminal. Live only, never persisted:
/// durable identity is the coordinator's.
pub const AgentRow = struct {
    /// The terminal the row hangs under.
    parent: TerminalRef,
    /// The session's own identity; it addresses no surface.
    resource: TerminalRef,
    /// Provider slug, e.g. `claude`. Empty when the catalog named none.
    provider_name: []const u8,
    native_id: []const u8,
    state: support.AgentState,

    /// Whether this row is the reason its terminal is asking for attention.
    pub fn needsAttention(row: AgentRow) bool {
        return row.state.needsAttention();
    }
};

/// The agent rows under one terminal, in catalog order, bounded by `out`.
/// Borrowed text is valid until the next provider drain, like every other
/// projection in this file.
pub fn agentRowsUnder(model: *const Model, id: TerminalRef, out: []AgentRow) usize {
    if (comptime !support.phux_enabled) return 0;
    var sessions: [model_module.max_agent_sessions]*const model_module.AgentSession = undefined;
    const limit = @min(out.len, sessions.len);
    if (limit == 0) return 0;
    const count = model.agentSessionsUnder(id, sessions[0..limit]);
    for (sessions[0..count], out[0..count]) |session, *row| {
        row.* = .{
            .parent = id,
            .resource = session.ref(),
            .provider_name = session.provider_name,
            .native_id = session.native_id,
            .state = session.state(),
        };
    }
    return count;
}

/// Every agent row under one tab's terminals, in pane order then catalog
/// order. This is the sidebar/topology shape: a tab, then the agents running
/// inside it, each stated with its provider and its state.
pub fn tabAgentRows(model: *const Model, workspace: *const Workspace, index: usize, out: []AgentRow) usize {
    if (comptime !support.phux_enabled) return 0;
    const current = workspace.treeConst(index) orelse return 0;
    var refs: [layout.max_panes]TerminalRef = undefined;
    const pane_count = current.terminals(&refs);
    var written: usize = 0;
    for (refs[0..pane_count]) |ref| {
        if (written == out.len) break;
        written += agentRowsUnder(model, ref, out[written..]);
    }
    return written;
}

/// Whether the trailing chrome status has something app-wide or
/// workspace-wide to say. Each latch needs the band even at one terminal:
/// otherwise its user action or preservation boundary would be silent.
fn chromeNoticeVisible(model: *const Model, workspace: *const Workspace) bool {
    if (limitNoticeVisible(model, workspace)) return true;
    if (model.state.rejectedExisting()) return true;
    if (saveFailureNoticeVisible(model)) return true;
    return model.phux_connection_unavailable;
}

pub fn chromeRevealedIn(model: *const Model, workspace: *const Workspace) bool {
    if (!model.config.hide_chrome_when_single) return true;
    if (workspace.tab_count > 1) return true;
    if (chromeNoticeVisible(model, workspace)) return true;
    if (workspaceTerminalRef(model, workspace) == null) return true;
    for (0..workspace.tab_count) |index| {
        const current = workspace.treeConst(index) orelse continue;
        var refs: [layout.max_panes]TerminalRef = undefined;
        const count = current.terminals(&refs);
        for (refs[0..count]) |id| {
            if (terminalNeedsAttention(model, id)) return true;
        }
    }
    return false;
}

/// The focused terminal of ONE workspace, filtered to a ref a provider still
/// vouches for. `Model.selectedTerminalRef` is this over the ACTIVE window;
/// the per-window painter and view need it over the window they are drawing.
pub fn workspaceTerminalRef(model: *const Model, workspace: *const Workspace) ?TerminalRef {
    const id = workspace.focusedTerminalRef() orelse return null;
    if (model.attachmentPending(id)) return null;
    if (support.providerKind(id) == .local) return if (model.provider.contains(id)) id else null;
    // Presence, not owner currency: a frozen publication keeps its pane.
    const remote = workspaceRemote(model, workspace) orelse return null;
    return if (remote.contains(id)) id else null;
}

/// The connection `workspace`'s focused pane belongs to: its selected tree's
/// exact attachment. The global ref lookup is ambiguous once two attachments
/// of one coordinator are on screen, and then neither window found its pane.
fn workspaceRemote(model: *const Model, workspace: *const Workspace) ?*const support.PhuxProvider {
    const tree = workspace.selectedTreeConst() orelse return null;
    return model.phuxForTreeConst(tree);
}

/// The search band takes its room from the content rect rather than
/// overlaying the grid, so painter, hit testing and PTY sizing agree.
pub const search_bar_height: f32 = chrome_band_height;

pub fn searchRevealedIn(model: *const Model, workspace: *const Workspace) bool {
    const terminal_ref = workspaceTerminalRef(model, workspace) orelse return false;
    if (model.provider.terminalConst(terminal_ref)) |pane| return pane.session.search.open;
    const remote = workspaceRemote(model, workspace) orelse return false;
    const owner = remote.owner(terminal_ref) orelse return false;
    const state = model.remoteUiForOwnerConst(owner) orelse return false;
    return state.search.open;
}

pub const config_notice_height: f32 = search_bar_height;

/// Whether the config band is up; app-wide, not per window.
pub fn configNoticeRevealed(model: *const Model) bool {
    return model.configNoticeVisible();
}

/// The longest line `configNoticeLine` can produce.
pub const config_notice_bytes: usize = @max(
    // "Config line 4294967295: understood, but does nothing in this build 'x…'"
    "Config line 4294967295: ".len + longest_summary + " ''".len + config_module.max_diagnostic_text_bytes,
    // "Config: 16 lines were not applied (lines 4294967295, …)"
    "Config: 16 lines were not applied (lines )".len +
        config_module.max_diagnostics * "4294967295, ".len,
);

const longest_summary = blk: {
    var longest: usize = 0;
    for (std.enums.values(config_module.Diagnostic.Kind)) |kind| {
        longest = @max(longest, kind.summary().len);
    }
    break :blk longest;
};

/// The config band's one line, naming line numbers (and the problem when
/// there is only one). `out` must be `config_notice_bytes` long.
pub fn configNoticeLine(model: *const Model, out: []u8) []const u8 {
    const notes = model.config.diagnosticSlice();
    if (notes.len == 0) return "";
    var writer = std.Io.Writer.fixed(out);
    if (notes.len == 1) {
        const only = notes[0];
        // A missing separator's text is the whole line; do not quote it back.
        const detail = if (only.kind == .missing_separator) "" else only.text();
        if (detail.len == 0) {
            writer.print("Config line {d}: {s}", .{ only.line, only.kind.summary() }) catch {};
        } else {
            writer.print("Config line {d}: {s} '{s}'", .{ only.line, only.kind.summary(), detail }) catch {};
        }
        return writer.buffered();
    }
    writer.print("Config: {d} lines were not applied (lines ", .{notes.len}) catch {};
    for (notes, 0..) |diagnostic, index| {
        writer.print("{s}{d}", .{ if (index == 0) "" else ", ", diagnostic.line }) catch {};
    }
    writer.print(")", .{}) catch {};
    return writer.buffered();
}

pub const WorkspaceChrome = struct {
    titlebar_height: f32,
    /// Each band is zero-height when not shown.
    header: geometry.RectF,
    notice: geometry.RectF,
    search: geometry.RectF,
    content: geometry.RectF,
};

pub const PaletteEntry = model_module.PaletteDestination;

/// Sessions come in host groups, one per coordinator: this Mac's first,
/// then each registered host, whichever of them is active.
const PaletteStage = enum { placed, available, sessions, done };

/// One host group: the active coordinator's, or the peer in a slot.
pub const Group = union(enum) { active, peer: usize };

/// The host groups in switcher order: this Mac's coordinator first, then
/// registered hosts, each pass in active-then-slot order.
pub const CoordinatorGroups = struct {
    model: *const Model,
    remote_pass: bool = false,
    active_seen: bool = false,
    slot: usize = 0,
    done: bool = false,

    pub fn next(self: *CoordinatorGroups) ?Group {
        if (comptime !support.phux_enabled) return null;
        if (self.done) return null;
        if (self.nextInPass()) |group| return group;
        if (self.remote_pass) {
            self.done = true;
            return null;
        }
        self.remote_pass = true;
        self.active_seen = false;
        self.slot = 0;
        return self.nextInPass();
    }

    fn nextInPass(self: *CoordinatorGroups) ?Group {
        if (!self.active_seen) {
            self.active_seen = true;
            if (self.model.phuxConst()) |active| {
                if ((active.remoteTarget() != null) == self.remote_pass) return .active;
            }
        }
        while (self.slot < self.model.peers.items.len) {
            const slot = self.slot;
            self.slot += 1;
            const peer = self.model.phuxPeerAtConst(slot) orelse continue;
            if ((peer.remoteTarget() != null) == self.remote_pass) return .{ .peer = slot };
        }
        return null;
    }
};

/// What a peer's group is called: its registered host's label, else This Mac.
pub fn peerHostLabel(model: *const Model, coordinator: support.ProviderId) []const u8 {
    if (comptime !support.phux_enabled) return "This Mac";
    const slot = model.peerSlot(coordinator) orelse return "This Mac";
    const peer = model.phuxPeerAtConst(slot).?;
    return peer.remoteLabel() orelse "This Mac";
}

/// The showing peer whose pane is focused, whose unplaced terminals are the
/// available inventory; null when the focused pane is the active
/// coordinator's, a local one, or none.
fn inventoryPeer(model: *const Model) ?*const support.PhuxProvider {
    if (comptime !support.phux_enabled) return null;
    const ref = model.focusedTerminalRef() orelse return null;
    if (support.providerKind(ref) != .phux or model.activeOwnsRef(ref)) return null;
    const peer = model.phuxForRefConst(ref) orelse return null;
    if (!peer.showing() or peer.state() != .attached) return null;
    return peer;
}

/// A peer with nothing to list that is not simply connected with no
/// sessions: failed, or still connecting. Its group shows one row saying so.
fn peerDegraded(model: *const Model, slot: usize) bool {
    if (comptime !support.phux_enabled) return false;
    const peer = model.phuxPeerAtConst(slot) orelse return false;
    if (model.peers.items[slot].failed) return true;
    const state = peer.state();
    return state != .negotiated and state != .attached;
}

pub const PaletteIterator = struct {
    model: *const Model,
    needle: []const u8,
    stage: PaletteStage = .placed,
    window_index: usize = 0,
    tab_index: usize = 0,
    pane_index: usize = 0,
    remote_index: usize = 0,
    session_index: usize = 0,
    group: ?Group = null,
    groups: CoordinatorGroups,

    pub fn init(model: *const Model, workspace: *const Workspace) PaletteIterator {
        return .{ .model = model, .needle = workspace.palette.needle(), .groups = .{ .model = model } };
    }

    fn accepts(iterator: *const PaletteIterator, entry: PaletteEntry) bool {
        return iterator.needle.len == 0 or
            paletteDestinationMatches(iterator.model, entry, iterator.needle);
    }

    fn nextPlaced(iterator: *PaletteIterator) ?PaletteEntry {
        while (iterator.window_index < model_module.max_windows) {
            if (!iterator.model.windowOpen(iterator.window_index)) {
                iterator.window_index += 1;
                continue;
            }
            const workspace = iterator.model.wsAtConst(iterator.window_index) orelse {
                iterator.window_index += 1;
                continue;
            };
            if (iterator.tab_index >= workspace.tab_count) {
                iterator.window_index += 1;
                iterator.tab_index = 0;
                iterator.pane_index = 0;
                continue;
            }
            const tree = workspace.treeConst(iterator.tab_index) orelse {
                iterator.tab_index += 1;
                iterator.pane_index = 0;
                continue;
            };
            var refs: [layout.max_panes]TerminalRef = undefined;
            const count = tree.terminals(&refs);
            if (iterator.pane_index >= count) {
                iterator.tab_index += 1;
                iterator.pane_index = 0;
                continue;
            }
            const terminal_ref = refs[iterator.pane_index];
            iterator.pane_index += 1;
            const entry: PaletteEntry = .{ .placed_terminal = .{
                .window = @intCast(iterator.window_index),
                .tab = @intCast(iterator.tab_index),
                .terminal_ref = terminal_ref,
            } };
            if (iterator.accepts(entry)) return entry;
        }
        return null;
    }

    /// The available inventory follows the focused pane's coordinator: a
    /// showing peer's own unplaced terminals of the session it shows while
    /// one of its panes is focused, else the active coordinator's.
    fn nextAvailable(iterator: *PaletteIterator) ?PaletteEntry {
        if (inventoryPeer(iterator.model)) |peer| return iterator.nextPeerAvailable(peer);
        const refs = iterator.model.remoteTerminalRefs();
        while (iterator.remote_index < refs.len) {
            const terminal_ref = refs[iterator.remote_index];
            iterator.remote_index += 1;
            if (iterator.model.locateTerminal(terminal_ref) != null) continue;
            const entry: PaletteEntry = .{ .available_terminal = terminal_ref };
            if (iterator.accepts(entry)) return entry;
        }
        return null;
    }

    fn nextPeerAvailable(iterator: *PaletteIterator, peer: *const support.PhuxProvider) ?PaletteEntry {
        const terminals = peer.catalogTerminals();
        const session = peer.selectedSessionId() orelse return null;
        while (iterator.remote_index < terminals.len) {
            const terminal = terminals[iterator.remote_index];
            iterator.remote_index += 1;
            if (terminal.session_id != session) continue;
            if (iterator.model.locateTerminal(terminal.terminal_ref) != null) continue;
            const entry: PaletteEntry = .{ .available_terminal = terminal.terminal_ref };
            if (iterator.accepts(entry)) return entry;
        }
        return null;
    }

    fn nextSession(iterator: *PaletteIterator) ?PaletteEntry {
        const remote = iterator.model.phuxConst() orelse return null;
        const sessions = remote.sessionCatalog();
        while (iterator.session_index < sessions.len) {
            const entry: PaletteEntry = .{ .session = sessions[iterator.session_index].id };
            iterator.session_index += 1;
            if (iterator.accepts(entry)) return entry;
        }
        return null;
    }

    fn nextPeerSession(iterator: *PaletteIterator, slot: usize) ?PaletteEntry {
        const peer = iterator.model.phuxPeerAtConst(slot) orelse return null;
        const coordinator = peer.providerId();
        const sessions = peer.standbyCatalog();
        if (sessions.len == 0) {
            // A peer that cannot list is still a group: one row says why.
            if (iterator.session_index != 0 or !peerDegraded(iterator.model, slot)) return null;
            iterator.session_index = 1;
            const entry: PaletteEntry = .{ .peer_unavailable = coordinator };
            return if (iterator.accepts(entry)) entry else null;
        }
        while (iterator.session_index < sessions.len) {
            const entry: PaletteEntry = .{ .peer_session = .{ .coordinator = coordinator, .id = sessions[iterator.session_index].id, .attachment_id = peer.context_id } };
            iterator.session_index += 1;
            if (iterator.accepts(entry)) return entry;
        }
        return null;
    }

    fn nextGrouped(iterator: *PaletteIterator) ?PaletteEntry {
        while (true) {
            if (iterator.group == null) iterator.group = iterator.groups.next();
            const group = iterator.group orelse return null;
            const found = switch (group) {
                .active => iterator.nextSession(),
                .peer => |slot| iterator.nextPeerSession(slot),
            };
            if (found) |entry| return entry;
            iterator.group = null;
            iterator.session_index = 0;
        }
    }

    pub fn next(iterator: *PaletteIterator) ?PaletteEntry {
        while (true) {
            switch (iterator.stage) {
                .placed => if (iterator.nextPlaced()) |entry| return entry else {
                    iterator.stage = .available;
                },
                .available => if (iterator.nextAvailable()) |entry| return entry else {
                    iterator.stage = .sessions;
                },
                .sessions => if (iterator.nextGrouped()) |entry| return entry else {
                    iterator.stage = .done;
                },
                .done => return null,
            }
        }
    }
};

/// The most rows the switcher draws at once (it still reaches every match);
/// eight fits the minimum window height.
pub const palette_max_visible_rows: usize = 8;

/// The contiguous run of matches the switcher draws, always containing the
/// cursor.
pub const PaletteWindow = struct {
    first: usize = 0,
    count: usize = 0,

    pub fn contains(window: PaletteWindow, offset: usize) bool {
        return offset >= window.first and offset < window.first + window.count;
    }
};

/// Materialize only the visible selectable rows. The total working set may be
/// hundreds of large stable identities; caller storage stays capped at eight.
pub fn paletteEntriesWindowIn(
    model: *const Model,
    workspace: *const Workspace,
    window: PaletteWindow,
    out: []PaletteEntry,
) usize {
    var iterator = PaletteIterator.init(model, workspace);
    var index: usize = 0;
    var written: usize = 0;
    const limit = @min(window.count, out.len);
    while (iterator.next()) |entry| {
        if (index < window.first) {
            index += 1;
            continue;
        }
        if (written >= limit) break;
        out[written] = entry;
        written += 1;
        index += 1;
    }
    return written;
}

pub fn paletteDestinationMatches(model: *const Model, entry: PaletteEntry, needle: []const u8) bool {
    return switch (entry) {
        .placed_terminal => |placed| placedDestinationMatches(model, placed, needle),
        .available_terminal => |terminal_ref| terminalDestinationMatches(model, terminal_ref, needle),
        .session => |session_id| sessionDestinationMatches(model, session_id, needle),
        .peer_session => |target| peerSessionMatches(model, target, needle),
        .peer_unavailable => |coordinator| needle.len == 0 or containsIgnoreCase(peerHostLabel(model, coordinator), needle),
    };
}

/// A peer coordinator's session matches by its name or by its host group's
/// label, so typing a host name narrows the switcher to that host.
fn peerSessionMatches(model: *const Model, target: model_module.PeerSession, needle: []const u8) bool {
    if (needle.len == 0) return true;
    if (comptime !support.phux_enabled) return false;
    if (containsIgnoreCase(peerHostLabel(model, target.coordinator), needle)) return true;
    const slot = model.peerSlot(target.coordinator) orelse return false;
    const peer = model.phuxPeerAtConst(slot).?;
    for (peer.standbyCatalog()) |session| {
        if (session.id == target.id) return containsIgnoreCase(session.name, needle);
    }
    return false;
}

fn placedDestinationMatches(
    model: *const Model,
    placed: model_module.PlacedTerminalDestination,
    needle: []const u8,
) bool {
    var address: [32]u8 = undefined;
    const position = std.fmt.bufPrint(
        &address,
        "window {d} tab {d}",
        .{ placed.window + 1, placed.tab + 1 },
    ) catch "";
    return containsIgnoreCase(position, needle) or
        terminalDestinationMatches(model, placed.terminal_ref, needle);
}

fn terminalDestinationMatches(model: *const Model, terminal_ref: TerminalRef, needle: []const u8) bool {
    if (support.providerKind(terminal_ref) == .local) return localDestinationMatches(model, terminal_ref, needle);
    if (containsIgnoreCase("phux remote", needle)) return true;
    if (containsIgnoreCase(terminal_ref.terminal_id.phux.host(), needle)) return true;
    if (catalogDestinationMatches(model, terminal_ref, needle)) return true;
    const presentation = model.remotePresentation(terminal_ref) orelse return false;
    return containsIgnoreCase(presentation.title, needle) or
        containsIgnoreCase(@tagName(presentation.phase), needle);
}

fn localDestinationMatches(model: *const Model, ref: TerminalRef, needle: []const u8) bool {
    if (containsIgnoreCase("local native", needle)) return true;
    const pane = model.provider.terminalConst(ref) orelse return false;
    return containsIgnoreCase(pane.title(), needle) or containsIgnoreCase(pane.pwd(), needle);
}

fn catalogDestinationMatches(model: *const Model, ref: TerminalRef, needle: []const u8) bool {
    const remote = model.phuxForRefConst(ref) orelse return false;
    for (remote.catalogTerminals()) |*entry| {
        if (!entry.terminal_ref.eql(ref)) continue;
        return containsIgnoreCase(entry.title.slice(), needle) or containsIgnoreCase(entry.cwd.slice(), needle);
    }
    return false;
}

fn sessionDestinationMatches(model: *const Model, session_id: u32, needle: []const u8) bool {
    const remote = model.phuxConst() orelse return false;
    for (remote.sessionCatalog()) |session| {
        if (session.id != session_id) continue;
        var detail: [64]u8 = undefined;
        const searchable = std.fmt.bufPrint(
            &detail,
            "{d} {d} windows {d} clients",
            .{ session.id, session.window_count, session.attached_client_count },
        ) catch "";
        return containsIgnoreCase(session.name, needle) or containsIgnoreCase(searchable, needle);
    }
    return false;
}

pub fn containsIgnoreCase(haystack: []const u8, needle: []const u8) bool {
    if (needle.len == 0) return true;
    if (needle.len > haystack.len) return false;
    var start: usize = 0;
    while (start + needle.len <= haystack.len) : (start += 1) {
        var matched = true;
        for (needle, 0..) |want, offset| {
            if (std.ascii.toLower(haystack[start + offset]) != std.ascii.toLower(want)) {
                matched = false;
                break;
            }
        }
        if (matched) return true;
    }
    return false;
}

/// Band extents change in a step, never eased: painter, hit testing and PTY
/// sizing all derive from this, and an eased `content` height would send a
/// SIGWINCH per row crossed. Band contents may animate freely.
pub fn workspaceChrome(model: *const Model, size: geometry.SizeF) WorkspaceChrome {
    return workspaceChromeIn(model, model.wsConst(), size);
}

pub fn workspaceChromeIn(model: *const Model, workspace: *const Workspace, size: geometry.SizeF) WorkspaceChrome {
    if (workspace.shipping_terminal_space) |space| return shippingChromeIn(model, workspace, space);
    const inset = windowPadding(model);
    const titlebar = @max(inset, workspace.chrome_top + 4);
    const revealed = chromeRevealedIn(model, workspace);
    const side_extent = if (revealed and model.tab_placement == .side) side_rail_width + side_rail_gap else 0;
    const top_extent = if (revealed and model.tab_placement == .top and !tabsRideTitlebarIn(model, workspace))
        header_height
    else
        0;
    // The config band sits above search, which stays adjacent to its grid.
    const notice_extent = if (configNoticeRevealed(model)) config_notice_height else 0;
    const search_extent = if (searchRevealedIn(model, workspace)) search_bar_height else 0;
    const body_width = @max(0, size.width - inset * 2 - side_extent);
    return .{
        .titlebar_height = titlebar,
        .header = geometry.RectF.init(
            inset,
            titlebar,
            @max(0, size.width - inset * 2),
            top_extent,
        ),
        .notice = geometry.RectF.init(
            inset + side_extent,
            titlebar + top_extent,
            body_width,
            notice_extent,
        ),
        .search = geometry.RectF.init(
            inset + side_extent,
            titlebar + top_extent + notice_extent,
            body_width,
            search_extent,
        ),
        .content = geometry.RectF.init(
            inset + side_extent,
            titlebar + top_extent + notice_extent + search_extent,
            body_width,
            @max(0, size.height - titlebar - top_extent - notice_extent - search_extent - inset),
        ),
    };
}

/// The compiled chrome is measured independently of the terminal layer. The
/// remaining bands and all three terminal consumers share this one derivation.
fn shippingChromeIn(model: *const Model, workspace: *const Workspace, space: geometry.RectF) WorkspaceChrome {
    const inset = windowPadding(model);
    const x = space.x + @min(inset, space.width / 2);
    const y = space.y + @min(inset, space.height / 2);
    const width = @max(0, space.width - inset * 2);
    const height = @max(0, space.height - inset * 2);
    const notice = if (configNoticeRevealed(model)) @min(height, config_notice_height) else 0;
    const search = if (searchRevealedIn(model, workspace)) @min(height - notice, search_bar_height) else 0;
    return .{
        .titlebar_height = space.y,
        .header = .init(0, 0, space.width, space.y),
        .notice = .init(x, y, width, notice),
        .search = .init(x, y + notice, width, search),
        .content = .init(x, y + notice + search, width, height - notice - search),
    };
}

/// The one pane geometry derivation shared by painter, hit targets and PTY
/// sizing.
pub fn resolvePanes(model: *const Model, size: geometry.SizeF, out: []layout.Pane) usize {
    return resolvePanesIn(model, model.wsConst(), size, out);
}

pub fn resolvePanesIn(model: *const Model, workspace: *const Workspace, size: geometry.SizeF, out: []layout.Pane) usize {
    const current = workspace.selectedTreeConst() orelse return 0;
    return current.resolve(
        workspaceChromeIn(model, workspace, size).content,
        split_divider_width,
        split_pane_min_width,
        split_pane_min_height,
        out,
    );
}

pub const PaneViewport = struct {
    owner: ?@import("provider_contract").ReplicaOwner = null,
    terminal: support.TerminalRef,
    cols: u16,
    rows: u16,
};

/// Every pane's proposed grid for a surface of `size`, in resolve order.
pub const ProposedViewports = struct {
    items: [layout.max_panes]PaneViewport = undefined,
    count: usize = 0,
    /// A local pane was not yet measured; propose nothing.
    incomplete: bool = false,

    pub fn slice(self: *const ProposedViewports) []const PaneViewport {
        return self.items[0..self.count];
    }
};

pub fn proposedViewportsIn(
    model: *const Model,
    workspace: *const Workspace,
    size: geometry.SizeF,
) ProposedViewports {
    var result: ProposedViewports = .{};
    var panes: [layout.max_panes]layout.Pane = undefined;
    const count = resolvePanesIn(model, workspace, size, &panes);
    // Local panes use the painter's measured cell; this estimate is only for
    // remote panes, which have no local session.
    const metrics = terminalCellMetricsFor(terminalTokens(model));
    for (panes[0..count]) |pane| {
        const inner = paneGridRect(pane.rect, count);
        if (inner.width <= 0 or inner.height <= 0) continue;
        if (model.provider.terminalConst(pane.terminal)) |terminal| {
            const session = terminal.session;
            // Unmeasured: a guessed proposal would SIGWINCH the shell twice.
            const cell = session.measuredCell() orelse {
                result.incomplete = true;
                return result;
            };
            const proposed = grid.Session.clampGrid(
                @intFromFloat(@max(2, inner.width / cell.width)),
                @intFromFloat(@max(2, inner.height / cell.height)),
            );
            result.items[result.count] = .{ .terminal = pane.terminal, .cols = proposed.x, .rows = proposed.y };
            result.count += 1;
            continue;
        }
        const tree = workspace.selectedTreeConst() orelse continue;
        const presentation = model.remotePaintPresentationIn(tree, pane.terminal) orelse continue;
        if (presentation.phase != .live) continue;
        const proposed = grid.Session.clampGrid(
            @intFromFloat(@max(2, inner.width / metrics.width)),
            @intFromFloat(@max(2, inner.height / metrics.height)),
        );
        result.items[result.count] = .{ .terminal = pane.terminal, .owner = presentation.owner, .cols = proposed.x, .rows = proposed.y };
        result.count += 1;
    }
    return result;
}

/// The pane under a view point, or null over the band, a divider, or a
/// gutter. Same bounds and same clamps as `resolvePanes` by construction.
pub fn paneAtPoint(model: *const Model, size: geometry.SizeF, x: f32, y: f32) ?layout.Pane {
    const current = model.selectedTreeConst() orelse return null;
    return current.paneAt(
        workspaceChrome(model, size).content,
        split_divider_width,
        split_pane_min_width,
        split_pane_min_height,
        x,
        y,
    );
}

/// The resolved pane rects in layout order, zero-filled past the live count.
pub fn paneFrames(model: *const Model, size: geometry.SizeF) [layout.max_panes]geometry.RectF {
    var frames = [_]geometry.RectF{.{}} ** layout.max_panes;
    var panes: [layout.max_panes]layout.Pane = undefined;
    const count = resolvePanes(model, size, &panes);
    for (panes[0..count], 0..) |pane, index| frames[index] = pane.rect;
    return frames;
}

pub fn paneFrameFor(model: *const Model, size: geometry.SizeF, id: TerminalRef) ?geometry.RectF {
    var panes: [layout.max_panes]layout.Pane = undefined;
    const count = resolvePanes(model, size, &panes);
    for (panes[0..count]) |pane| {
        if (pane.terminal.eql(id)) return paneGridRect(pane.rect, count);
    }
    return null;
}

test "paneGridRect is identity for a lone pane and a constant inset for splits" {
    const testing = std.testing;
    const card = geometry.RectF.init(10, 20, 400, 300);
    try testing.expectEqualDeep(card, paneGridRect(card, 1));
    const grid_rect = paneGridRect(card, 2);
    try testing.expectEqual(card.x + pane_chrome_inset, grid_rect.x);
    try testing.expectEqual(card.y + pane_chrome_inset, grid_rect.y);
    try testing.expectEqual(card.width - 2 * pane_chrome_inset, grid_rect.width);
    try testing.expectEqual(card.height - 2 * pane_chrome_inset, grid_rect.height);
    try testing.expectEqualDeep(grid_rect, paneGridRect(card, 8));
}

test "pane chrome inset clears the rounded-corner overshoot" {
    const testing = std.testing;
    const tokens = baseTokens();
    const overshoot = tokens.radius.md * (1.0 - 1.0 / std.math.sqrt(@as(f32, 2.0)));
    try testing.expect(pane_chrome_inset > overshoot);
    try testing.expectEqual(chrome_band_inset, pane_chrome_inset);
}
