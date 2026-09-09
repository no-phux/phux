const std = @import("std");
const local = @import("../providers/local/provider.zig");
const layout = @import("layout.zig");
const support = @import("phux_support.zig");
pub const attachments = @import("attachment_state.zig");

pub const LocalTerminalId = support.LocalTerminalId;
pub const TerminalRef = support.TerminalRef;
pub const max_terminals = local.max_terminals;

/// Tabs per window. A tab owns a `layout.Tree`; the tree owns up to
/// `layout.max_panes` terminals. Both ceilings are structural, not policy.
pub const max_tabs: usize = 16;

pub const TabPlacement = enum { top, side };

pub const SurfaceSelection = union(enum) {
    terminal: TerminalRef,
    web,

    pub fn eql(a: SurfaceSelection, b: SurfaceSelection) bool {
        return switch (a) {
            .terminal => |id| switch (b) {
                .terminal => |other| id.eql(other),
                .web => false,
            },
            .web => switch (b) {
                .web => true,
                .terminal => false,
            },
        };
    }
};

/// v5 adds bounded remote attachment references. v4 carries windows. v3 was
/// one window's tree schema plus per-terminal
/// working directories; v2 was the same tree with no directories; v1 encoded a
/// fixed two-pane workspace (`layout: {single,split}`, `attachments[2]`, one
/// `split_fraction`, `focused_attachment`) and cannot express a tab that owns a
/// nested tree, so it is MIGRATED, not read: every v1 terminal becomes its own
/// tab, and a v1 split becomes a horizontal split inside the focused tab.
///
/// v3 -> v4 is the same shape one level up: a pre-multi-window file describes
/// exactly one window, so it migrates to a single `SnapshotWindow` owning
/// every tab it carried.
pub const topology_snapshot_version: u16 = 5;

/// Windows one snapshot can carry — the model's own ceiling (the scene's
/// window plus the toolkit's four declared ones).
pub const max_snapshot_windows: usize = 5;

/// Tabs one snapshot can carry ACROSS every window.
///
/// `max_tabs` is the per-window ceiling. This whole-session ceiling matches
/// the combined local and remote leaf budget, preserving the v4 bound while
/// allowing every leaf to be remote.
pub const max_snapshot_tabs: usize = max_terminals;

/// Local restoration recreates shells, never local process state. Remote
/// restoration retains references pending explicit provider/context evidence.
pub const process_restoration_supported = false;

/// Byte ceiling for one persisted working directory.
///
/// Deliberately far below `std.fs.max_path_bytes` (1024): the snapshot is
/// passed BY VALUE through migration and validation, and a full-length path
/// per registry slot would put half a megabyte on the stack for a field whose
/// realistic occupancy is a few dozen bytes. A path past the ceiling is simply
/// not recorded, which degrades to "this pane opens in $HOME" — the behaviour
/// of a pane whose shell never reported OSC 7.
pub const max_snapshot_cwd_bytes: usize = 256;

/// One terminal's persisted working directory. Empty means UNKNOWN — the
/// shell never reported one, or it was past the ceiling — never "/".
pub const SnapshotCwd = struct {
    bytes: [max_snapshot_cwd_bytes]u8 = @splat(0),
    len: u16 = 0,

    pub fn slice(cwd: *const SnapshotCwd) []const u8 {
        if (cwd.len > max_snapshot_cwd_bytes) return "";
        return cwd.bytes[0..cwd.len];
    }

    /// Record `path`, or record nothing when it cannot be honoured on the way
    /// back in. Rejects what `paneArgvIn` would reject anyway (relative paths)
    /// plus the two bytes the on-disk line format cannot round trip.
    pub fn set(cwd: *SnapshotCwd, path: []const u8) void {
        cwd.* = .{};
        if (path.len == 0 or path.len > max_snapshot_cwd_bytes) return;
        if (path[0] != '/') return;
        for (path) |byte| if (byte == 0 or byte == '\n') return;
        @memcpy(cwd.bytes[0..path.len], path);
        cwd.len = @intCast(path.len);
    }
};

pub const SnapshotSelection = union(enum) {
    tab: u8,
    web,

    pub fn eql(a: SnapshotSelection, b: SnapshotSelection) bool {
        return switch (a) {
            .tab => |index| switch (b) {
                .tab => |other| index == other,
                .web => false,
            },
            .web => b == .web,
        };
    }
};

/// One node of a serialized tree, in the same index space the live tree
/// uses. `terminal` is meaningful for leaves; `first`/`second`/`fraction`
/// for branches.
pub const SnapshotNode = struct {
    kind: layout.Kind = .free,
    parent: layout.NodeId = layout.none,
    terminal: LocalTerminalId = .terminal_1,
    has_terminal: bool = false,
    /// v5 remote reference table index. Local leaves keep their v2-v4 offset.
    remote_ref: ?u8 = null,
    orientation: layout.Orientation = .horizontal,
    fraction: f32 = 0.5,
    first: layout.NodeId = layout.none,
    second: layout.NodeId = layout.none,
};

/// A serialized tab: its whole tree plus which leaf held focus.
pub const SnapshotTab = struct {
    nodes: [layout.max_nodes]SnapshotNode = [_]SnapshotNode{.{}} ** layout.max_nodes,
    root: layout.NodeId = layout.none,
    focus: layout.NodeId = layout.none,
};

/// One serialized WINDOW: how many of the snapshot's tabs are its, and which
/// of them it had selected.
///
/// Tabs are stored once, in one flat array, laid out in window order — so a
/// window owns the CONTIGUOUS run starting after every earlier window's tabs.
/// There is deliberately no per-tab window tag: a tag and a count are two
/// encodings of one fact, and only one of them can be wrong.
pub const SnapshotWindow = struct {
    tab_count: u8 = 0,
    /// The selected tab, numbered WITHIN this window's run. Window-relative so
    /// a window's meaning does not depend on how many tabs the windows before
    /// it happened to have.
    selection: SnapshotSelection = .web,
};

/// The registry offset a terminal id occupies, or null when it is outside the
/// registry entirely. The cwd side table is indexed by this, so it is the one
/// place the id-to-slot arithmetic lives.
pub fn terminalOffset(id: LocalTerminalId) ?usize {
    const raw = @intFromEnum(id);
    if (raw < local.first_terminal_raw) return null;
    const offset = raw - local.first_terminal_raw;
    if (offset >= max_terminals) return null;
    return @intCast(offset);
}

pub const TopologySnapshot = struct {
    version: u16 = topology_snapshot_version,
    /// Windows, densely numbered from zero. Window 0 is the main (scene)
    /// window; a session that had only that one records `window_count == 1`,
    /// and a session with nothing open at all records zero.
    window_count: u8 = 0,
    windows: [max_snapshot_windows]SnapshotWindow = [_]SnapshotWindow{.{}} ** max_snapshot_windows,
    /// Every window's tabs, concatenated in window order.
    tab_count: u8 = 0,
    tabs: [max_snapshot_tabs]SnapshotTab = [_]SnapshotTab{.{}} ** max_snapshot_tabs,
    tab_placement: TabPlacement = .top,
    /// Working directories, indexed by REGISTRY OFFSET rather than by node.
    ///
    /// A cwd belongs to a terminal, not to the pane that happens to hold it,
    /// and `validate` already proves every leaf's terminal is a distinct
    /// registry offset — so a flat 32-entry table is both the honest shape and
    /// an order of magnitude smaller than one path per tree node (31 nodes x
    /// 16 tabs) would be.
    cwds: [max_terminals]SnapshotCwd = [_]SnapshotCwd{.{}} ** max_terminals,
    references: attachments.Table = .{},

    pub fn terminalRef(snapshot: *const TopologySnapshot, node: SnapshotNode) ?TerminalRef {
        if (!node.has_terminal) return null;
        if (node.remote_ref) |index| {
            const reference = snapshot.references.get(index) orelse return null;
            return reference.terminal_ref;
        }
        if (terminalOffset(node.terminal) == null) return null;
        return local.localRef(node.terminal);
    }

    pub fn cwdFor(snapshot: *const TopologySnapshot, id: LocalTerminalId) []const u8 {
        const offset = terminalOffset(id) orelse return "";
        return snapshot.cwds[offset].slice();
    }

    pub fn setCwd(snapshot: *TopologySnapshot, id: LocalTerminalId, path: []const u8) void {
        const offset = terminalOffset(id) orelse return;
        snapshot.cwds[offset].set(path);
    }

    /// The offset into `tabs` where window `index`'s run begins, or null when
    /// there is no such window. Deliberately computed rather than stored: an
    /// offset and a set of counts are two encodings of one fact.
    pub fn windowTabOffset(snapshot: *const TopologySnapshot, index: usize) ?usize {
        if (index >= snapshot.window_count) return null;
        var offset: usize = 0;
        for (snapshot.windows[0..index]) |window| offset += window.tab_count;
        return offset;
    }

    /// Window `index`'s own tabs.
    pub fn windowTabs(snapshot: *const TopologySnapshot, index: usize) []const SnapshotTab {
        const offset = snapshot.windowTabOffset(index) orelse return &.{};
        const count: usize = snapshot.windows[index].tab_count;
        if (offset + count > snapshot.tab_count) return &.{};
        return snapshot.tabs[offset..][0..count];
    }

    /// Taken BY POINTER, not by value: the snapshot carries thirty-two trees
    /// and a directory table, and a by-value validator put tens of kilobytes
    /// on the caller's frame at every save and every restore.
    pub fn validate(snapshot: *const TopologySnapshot) !void {
        if (snapshot.version != topology_snapshot_version) return error.UnsupportedTopologyVersion;
        if (snapshot.tab_count > max_snapshot_tabs) return error.InvalidTopology;
        if (snapshot.window_count > max_snapshot_windows) return error.InvalidTopology;
        try snapshot.references.validate();
        try snapshot.validateWindows();
        try snapshot.validateTabs();
        for (snapshot.cwds) |cwd| try validateCwd(cwd);
    }

    fn validateWindows(snapshot: *const TopologySnapshot) !void {
        const count: usize = snapshot.tab_count;

        // Every tab belongs to exactly one window, so the windows' counts must
        // account for the tab array exactly — a short sum would leave orphan
        // tabs no window restores, and a long one would read past the run.
        var counted: usize = 0;
        for (snapshot.windows[0..snapshot.window_count]) |window| {
            if (window.tab_count > max_tabs) return error.InvalidTopology;
            switch (window.selection) {
                .tab => |index| if (index >= window.tab_count) return error.InvalidTopology,
                .web => {},
            }
            counted += window.tab_count;
        }
        if (counted != count) return error.InvalidTopology;
        // Windows past the count must be blank, so a truncated write cannot be
        // read back as extra windows.
        for (snapshot.windows[snapshot.window_count..]) |window| {
            if (window.tab_count != 0 or window.selection != .web) return error.InvalidTopology;
        }
    }

    fn validateTabs(snapshot: *const TopologySnapshot) !void {
        var seen: SeenTerminals = .{};
        for (snapshot.tabs[0..snapshot.tab_count]) |*tab| try validateTab(snapshot, tab, &seen);
        // Every reference must be used; a missing leaf is a truncated topology.
        for (seen.remote[0..snapshot.references.count]) |used| if (!used) return error.InvalidTopology;
        for (snapshot.tabs[snapshot.tab_count..]) |tab| {
            if (tab.root != layout.none or tab.focus != layout.none) return error.InvalidTopology;
        }
    }
};

const SeenTerminals = struct {
    local: [max_terminals]bool = @splat(false),
    remote: [attachments.max_references]bool = @splat(false),
    count: usize = 0,

    fn admit(self: *SeenTerminals, snapshot: *const TopologySnapshot, node: SnapshotNode) !void {
        if (!node.has_terminal) return error.InvalidTopology;
        const seen = if (node.remote_ref) |index| remote: {
            _ = snapshot.references.get(index) orelse return error.InvalidTopology;
            break :remote &self.remote[index];
        } else local_seen: {
            const index = terminalOffset(node.terminal) orelse return error.InvalidTopology;
            break :local_seen &self.local[index];
        };
        if (seen.*) return error.InvalidTopology;
        seen.* = true;
        self.count += 1;
        if (self.count > max_terminals) return error.InvalidTopology;
    }
};

fn validateTab(snapshot: *const TopologySnapshot, tab: *const SnapshotTab, seen: *SeenTerminals) !void {
    try validateTabRoot(tab);
    var reachable: [layout.max_nodes]bool = @splat(false);
    try walkSnapshotNode(tab.*, tab.root, &reachable);
    if (!reachable[tab.focus]) return error.InvalidTopology;
    for (tab.nodes, 0..) |node, index| {
        if (node.kind != .leaf and node.remote_ref != null) return error.InvalidTopology;
        if (node.kind == .free) continue;
        if (!reachable[index]) return error.InvalidTopology;
        if (node.kind == .leaf) try seen.admit(snapshot, node);
    }
}

fn validateTabRoot(tab: *const SnapshotTab) !void {
    if (tab.root >= layout.max_nodes or tab.focus >= layout.max_nodes) return error.InvalidTopology;
    if (tab.nodes[tab.focus].kind != .leaf) return error.InvalidTopology;
    if (tab.nodes[tab.root].parent != layout.none) return error.InvalidTopology;
}

fn validateCwd(cwd: SnapshotCwd) !void {
    if (cwd.len > max_snapshot_cwd_bytes) return error.InvalidTopology;
    if (cwd.len == 0) return;
    const path = cwd.bytes[0..cwd.len];
    if (path[0] != '/') return error.InvalidTopology;
    for (path) |byte| if (byte == 0 or byte == '\n') return error.InvalidTopology;
}

fn walkSnapshotNode(tab: SnapshotTab, id: layout.NodeId, reachable: *[layout.max_nodes]bool) !void {
    if (id >= layout.max_nodes) return error.InvalidTopology;
    if (reachable[id]) return error.InvalidTopology; // a cycle or a shared child
    reachable[id] = true;
    const node = tab.nodes[id];
    switch (node.kind) {
        .free => return error.InvalidTopology,
        .leaf => {
            if (node.first != layout.none or node.second != layout.none) return error.InvalidTopology;
        },
        .branch => {
            try validateBranch(tab, id);
            try walkSnapshotNode(tab, node.first, reachable);
            try walkSnapshotNode(tab, node.second, reachable);
        },
    }
}

fn validateBranch(tab: SnapshotTab, id: layout.NodeId) !void {
    const node = tab.nodes[id];
    if (node.first >= layout.max_nodes or node.second >= layout.max_nodes) return error.InvalidTopology;
    if (!std.math.isFinite(node.fraction) or
        node.fraction < layout.min_fraction or node.fraction > layout.max_fraction) return error.InvalidTopology;
    if (tab.nodes[node.first].parent != id or tab.nodes[node.second].parent != id) return error.InvalidTopology;
}

/// The pre-tree schemas. v0 was a bare count-and-flag; v1 was the two-pane
/// attachment model. Both are readable only through migration.
pub const LegacyTopologySnapshotV0 = struct {
    terminal_count: u8 = 1,
    selected_index: u8 = 0,
    split: bool = false,
    split_fraction: f32 = 0.5,
};

pub const LegacyTopologySnapshotV1 = struct {
    terminal_count: u8 = 0,
    terminal_order: [4]LocalTerminalId = [_]LocalTerminalId{.terminal_1} ** 4,
    /// null selects the web surface, matching v1's `SnapshotSelection`.
    selection: ?LocalTerminalId = null,
    split: bool = false,
    split_fraction: f32 = 0.5,
    attachments: [2]?LocalTerminalId = .{ null, null },
    focused_attachment: u8 = 0,
    tab_placement: TabPlacement = .top,
};

/// v2: the tree schema before working directories. The TREE part is
/// byte-identical to v3 — `SnapshotTab` and `SnapshotNode` did not change —
/// so this carries the same tabs and differs only in having no cwd table.
pub const LegacyTopologySnapshotV2 = struct {
    tab_count: u8 = 0,
    tabs: [max_tabs]SnapshotTab = [_]SnapshotTab{.{}} ** max_tabs,
    selection: SnapshotSelection = .web,
    tab_placement: TabPlacement = .top,
};

/// v3: one window's tree schema plus working directories, the schema before
/// windows existed. Its whole content is window 0 of a v4 snapshot.
pub const LegacyTopologySnapshotV3 = struct {
    tab_count: u8 = 0,
    tabs: [max_tabs]SnapshotTab = [_]SnapshotTab{.{}} ** max_tabs,
    selection: SnapshotSelection = .web,
    tab_placement: TabPlacement = .top,
    cwds: [max_terminals]SnapshotCwd = [_]SnapshotCwd{.{}} ** max_terminals,
};

pub const PersistedTopologySnapshot = union(enum) {
    v0: LegacyTopologySnapshotV0,
    v1: LegacyTopologySnapshotV1,
    v2: LegacyTopologySnapshotV2,
    v3: LegacyTopologySnapshotV3,
    v4: TopologySnapshot,
    v5: TopologySnapshot,
};

/// Build a one-leaf tab. Every migration path lands here: a legacy terminal
/// had no pane structure of its own, so it becomes a tab holding one pane.
pub fn singleLeafTab(id: LocalTerminalId) SnapshotTab {
    var tab: SnapshotTab = .{};
    tab.nodes[0] = .{ .kind = .leaf, .parent = layout.none, .terminal = id, .has_terminal = true };
    tab.root = 0;
    tab.focus = 0;
    return tab;
}

/// Two leaves under one branch, for a migrated v1 split.
fn splitTab(first: LocalTerminalId, second: LocalTerminalId, fraction: f32, focus_second: bool) SnapshotTab {
    var tab: SnapshotTab = .{};
    tab.nodes[0] = .{ .kind = .leaf, .parent = 2, .terminal = first, .has_terminal = true };
    tab.nodes[1] = .{ .kind = .leaf, .parent = 2, .terminal = second, .has_terminal = true };
    tab.nodes[2] = .{
        .kind = .branch,
        .parent = layout.none,
        .orientation = .horizontal,
        .fraction = std.math.clamp(fraction, layout.min_fraction, layout.max_fraction),
        .first = 0,
        .second = 1,
    };
    tab.root = 2;
    tab.focus = if (focus_second) 1 else 0;
    return tab;
}

pub fn migrateTopologySnapshot(persisted: PersistedTopologySnapshot) !TopologySnapshot {
    const snapshot = switch (persisted) {
        .v5 => |current| current,
        .v4 => |legacy| blk: {
            var current = legacy;
            if (current.version == 4) current.version = topology_snapshot_version;
            break :blk current;
        },
        // v3 -> v4: a pre-multi-window file describes exactly ONE window, and
        // restoring it as one window is the whole compatibility claim. A file
        // with no tabs stays a zero-window snapshot, which is what "there is
        // nothing to reopen" has always meant.
        .v3 => |legacy| blk: {
            var migrated: TopologySnapshot = .{
                .tab_count = legacy.tab_count,
                .tab_placement = legacy.tab_placement,
                .cwds = legacy.cwds,
            };
            @memcpy(migrated.tabs[0..max_tabs], &legacy.tabs);
            if (legacy.tab_count > 0 or legacy.selection != .web) {
                migrated.window_count = 1;
                migrated.windows[0] = .{ .tab_count = legacy.tab_count, .selection = legacy.selection };
            }
            break :blk migrated;
        },
        // v2 -> v3 is purely additive: the tree arrives whole and NO working
        // directory was ever recorded, so every restored pane opens where a
        // pane whose shell never reported OSC 7 opens.
        .v2 => |legacy| try migrateTopologySnapshot(.{ .v3 = .{
            .tab_count = legacy.tab_count,
            .tabs = legacy.tabs,
            .selection = legacy.selection,
            .tab_placement = legacy.tab_placement,
        } }),
        .v1 => |legacy| try migrateV1(legacy),
        .v0 => |legacy| try migrateV1(try upgradeV0(legacy)),
    };
    try snapshot.validate();
    return snapshot;
}

fn upgradeV0(legacy: LegacyTopologySnapshotV0) !LegacyTopologySnapshotV1 {
    if (legacy.terminal_count > 4 or !std.math.isFinite(legacy.split_fraction)) return error.InvalidTopology;
    var v1: LegacyTopologySnapshotV1 = .{
        .terminal_count = legacy.terminal_count,
        .split = legacy.split and legacy.terminal_count >= 2,
        .split_fraction = std.math.clamp(legacy.split_fraction, 0.05, 0.95),
    };
    for (0..legacy.terminal_count) |index| v1.terminal_order[index] = @enumFromInt(local.first_terminal_raw + index);
    if (legacy.terminal_count == 0) return v1;
    const selected = @min(@as(usize, legacy.selected_index), legacy.terminal_count - 1);
    v1.selection = v1.terminal_order[selected];
    v1.attachments[0] = v1.terminal_order[selected];
    if (v1.split) v1.attachments[1] = v1.terminal_order[if (selected == 0) 1 else 0];
    return v1;
}

/// The tab a migrated legacy snapshot had selected, in the ONE window it
/// describes. A convenience for the migration tests and for anything that
/// wants "the main window's selection" without spelling the indirection.
pub fn primarySelection(snapshot: *const TopologySnapshot) SnapshotSelection {
    if (snapshot.window_count == 0) return .web;
    return snapshot.windows[0].selection;
}

/// v1 → v2: every terminal in the old tab order becomes its own tab. A v1
/// split had TWO terminals sharing the content area while both also appeared
/// in the tab strip; the tree model has no such duality, so the split's two
/// terminals merge into ONE tab holding a horizontal branch and the second
/// terminal loses its separate tab.
fn migrateV1(legacy: LegacyTopologySnapshotV1) !TopologySnapshot {
    if (legacy.terminal_count > legacy.terminal_order.len) return error.InvalidTopology;
    if (!std.math.isFinite(legacy.split_fraction)) return error.InvalidTopology;
    if (legacy.focused_attachment > 1) return error.InvalidTopology;
    const count: usize = legacy.terminal_count;
    for (legacy.terminal_order[0..count], 0..) |id, index| {
        const raw = @intFromEnum(id);
        if (raw < local.first_terminal_raw or raw >= std.math.maxInt(u64) - 1) return error.InvalidTopology;
        for (legacy.terminal_order[0..index]) |prior| if (prior == id) return error.InvalidTopology;
    }

    const split_pair: ?[2]LocalTerminalId = if (legacy.split and legacy.attachments[0] != null and legacy.attachments[1] != null)
        .{ legacy.attachments[0].?, legacy.attachments[1].? }
    else
        null;

    var migrated: TopologySnapshot = .{ .tab_placement = legacy.tab_placement };
    var tabs: u8 = 0;
    var selected: ?u8 = null;
    for (legacy.terminal_order[0..count]) |id| {
        if (split_pair) |pair| {
            // The split's second terminal is absorbed by the first's tab.
            if (id == pair[1]) continue;
            if (id == pair[0]) {
                migrated.tabs[tabs] = splitTab(
                    pair[0],
                    pair[1],
                    std.math.clamp(legacy.split_fraction, layout.min_fraction, layout.max_fraction),
                    legacy.focused_attachment == 1,
                );
                if (legacy.selection) |chosen| {
                    if (chosen == pair[0] or chosen == pair[1]) selected = tabs;
                }
                tabs += 1;
                continue;
            }
        }
        migrated.tabs[tabs] = singleLeafTab(id);
        if (legacy.selection) |chosen| {
            if (chosen == id) selected = tabs;
        }
        tabs += 1;
    }
    migrated.tab_count = tabs;
    // Every pre-window schema describes exactly one window.
    const selection: SnapshotSelection = if (selected) |index| .{ .tab = index } else .web;
    if (tabs > 0 or selection != .web) {
        migrated.window_count = 1;
        migrated.windows[0] = .{ .tab_count = tabs, .selection = selection };
    }
    return migrated;
}
