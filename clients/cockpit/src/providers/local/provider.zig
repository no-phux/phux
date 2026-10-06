const std = @import("std");
const builtin = @import("builtin");
const native_sdk = @import("native_sdk");
const vt = @import("ghostty-vt");
const grid = @import("../../terminal/grid.zig");
const provider_contract = @import("provider_contract");

pub const LocalResourceId = provider_contract.LocalResourceId;
pub const TerminalRef = provider_contract.TerminalRef;
pub const ReplicaOwner = provider_contract.ReplicaOwner;
pub const Phase = provider_contract.Phase;

/// The local registry ceiling (a flat array sized for every tab's panes).
pub const max_terminals: usize = 32;

/// How many local terminals can hold a live shell at once: the SDK's
/// process-wide pty table. A pane past it would be born dead (its spawn is
/// refused), so creation refuses instead. Derived from the SDK pin, never a
/// literal. Measure with `scripts/drive-shell-ceiling.sh`.
pub const max_live_shells: usize = native_sdk.max_effect_ptys;

pub const first_terminal_raw: u64 = @intFromEnum(LocalResourceId.terminal_1);
pub const clipboard_key: u64 = 100;
pub const paste_clipboard_key: u64 = 101;
pub const outbound_buffer_bytes: usize = 64 * 1024;

const default_shell: []const u8 = if (builtin.os.tag == .macos)
    "/bin/zsh"
else if (builtin.os.tag == .windows)
    "cmd.exe"
else
    "/bin/sh";

const default_shell_argv: []const []const u8 = if (builtin.os.tag == .windows)
    &.{default_shell}
else
    &.{ default_shell, "-i" };

/// Every local terminal spawns the same login shell.
const shell_argv: []const []const u8 = if (builtin.os.tag == .macos)
    &.{ "/bin/zsh", "-l", "-c", "cd \"$HOME\" && exec /bin/zsh -i" }
else
    default_shell_argv;

/// Storage and argv for a configured `shell` / `command`, run as a command
/// line (`exec`) by the login shell so `command = tmux attach` works. Unlike
/// an OSC 7 directory (untrusted, always quoted by `paneArgvIn`), this comes
/// from the user's own config, so it is not quoted.
pub const ShellCommand = struct {
    command: [max_cwd_command_bytes]u8 = undefined,
    slots: [4][]const u8 = undefined,
    len: usize = 0,

    pub fn isSet(self: *const ShellCommand) bool {
        return self.len != 0;
    }

    pub fn argv(self: *const ShellCommand) []const []const u8 {
        return self.slots[0..self.len];
    }

    /// Adopt `value`, keeping the built-in shell when it is empty, contains a
    /// NUL, or does not fit.
    pub fn set(self: *ShellCommand, value: []const u8) bool {
        self.len = 0;
        if (value.len == 0) return false;
        if (std.mem.indexOfScalar(u8, value, 0) != null) return false;
        const prefix = "exec ";
        if (prefix.len + value.len > self.command.len) return false;
        @memcpy(self.command[0..prefix.len], prefix);
        @memcpy(self.command[prefix.len..][0..value.len], value);
        const written = prefix.len + value.len;
        self.slots[0] = default_shell;
        self.slots[1] = "-l";
        self.slots[2] = "-c";
        self.slots[3] = self.command[0..written];
        self.len = 4;
        return true;
    }
};

pub fn localRef(id: LocalResourceId) TerminalRef {
    return .{ .provider_id = .local, .terminal_id = .{ .local = id } };
}

pub fn initialResourceId(index: usize) LocalResourceId {
    return @enumFromInt(first_terminal_raw + index);
}

pub fn initialTerminalRef(index: usize) TerminalRef {
    return localRef(initialResourceId(index));
}

pub fn ptyKey(index: usize) u64 {
    return 1 + index;
}

pub fn paneArgv(_: usize) []const []const u8 {
    return shell_argv;
}

/// Byte ceiling for the generated `cd ... ; exec ...` word, well inside the
/// SDK's 2048-byte total argv cap.
pub const max_cwd_command_bytes: usize = 1024;

/// Storage for one cwd-carrying argv; must outlive the `Pane.argv` slice.
pub const CwdArgv = struct {
    command: [max_cwd_command_bytes]u8 = undefined,
    slots: [4][]const u8 = undefined,
};

/// The argv for a shell that starts in `cwd` (the SDK spawn has no cwd field).
/// The path is single-quoted with `'\''` escapes, so a hostile directory name
/// from OSC 7 cannot become a command. A failed `cd` falls back to `$HOME`
/// rather than killing the shell. Returns the plain argv when `cwd` is empty,
/// relative, contains a NUL, or does not fit.
pub fn paneArgvIn(cwd: []const u8, out: *CwdArgv) []const []const u8 {
    if (builtin.os.tag == .windows) return shell_argv;
    if (cwd.len == 0 or cwd[0] != '/') return shell_argv;
    if (std.mem.indexOfScalar(u8, cwd, 0) != null) return shell_argv;

    const prefix = "cd '";
    const suffix = "' 2>/dev/null || cd \"$HOME\"; exec " ++ default_shell ++ " -i";
    var written: usize = prefix.len;
    @memcpy(out.command[0..prefix.len], prefix);
    for (cwd) |byte| {
        // A single quote is the ONE byte that ends the quoted word, so it
        // closes the word (`'`), passes an escaped literal quote (`\'`), and
        // reopens (`'`) — four bytes for one.
        const piece: []const u8 = if (byte == '\'') "'\\''" else (&byte)[0..1];
        if (written + piece.len + suffix.len > out.command.len) return shell_argv;
        @memcpy(out.command[written..][0..piece.len], piece);
        written += piece.len;
    }
    if (written + suffix.len > out.command.len) return shell_argv;
    @memcpy(out.command[written..][0..suffix.len], suffix);
    written += suffix.len;

    // `-l` only where the plain argv is already a login shell (macOS).
    out.slots[0] = default_shell;
    if (builtin.os.tag == .macos) {
        out.slots[1] = "-l";
        out.slots[2] = "-c";
        out.slots[3] = out.command[0..written];
        return out.slots[0..4];
    }
    out.slots[1] = "-c";
    out.slots[2] = out.command[0..written];
    return out.slots[0..3];
}

pub const Pane = struct {
    id: TerminalRef,
    session: *grid.Session,
    pty_key: u64 = 1,
    argv: []const []const u8 = default_shell_argv,
    phase: Phase = .starting,
    session_generation: u64 = 0,
    exit_code: i32 = 0,
    exit_signal: i32 = 0,
    exit_reason: native_sdk.EffectExitReason = .exited,
    cols: u16 = 80,
    rows: u16 = 24,
    selecting: bool = false,
    copied_bytes: u64 = 0,
    macos_natural_keys_held: u8 = 0,
    copy_failed: bool = false,
    scrollback_wheel_accum: f32 = 0,
    mouse_wheel_y_accum: f32 = 0,
    mouse_wheel_x_accum: f32 = 0,
    mouse_wheel_next_horizontal: bool = false,
    mouse_last_cell: ?vt.Coordinate = null,
    mouse_protocol_fingerprint: u8 = 0,
    output_batches: u64 = 0,
    output_bytes: u64 = 0,
    write_refusals: u32 = 0,
    write_refusals_total: u32 = 0,
    native_delivery_failures: u32 = 0,
    outbound_buffer: [outbound_buffer_bytes]u8 = undefined,
    outbound_head: usize = 0,
    outbound_len: usize = 0,
    outbound_dropped: u64 = 0,
    /// Watermarks over the cumulative loss counters: attention means new loss
    /// since the operator last looked, while the counters stay evidence.
    acknowledged_outbound_dropped: u64 = 0,
    acknowledged_response_dropped: u64 = 0,
    acknowledged_write_refusals: u32 = 0,
    acknowledged_delivery_failures: u32 = 0,

    /// Loss the operator has not been shown yet.
    pub fn hasUnacknowledgedLoss(pane: *const Pane) bool {
        return pane.outbound_dropped > pane.acknowledged_outbound_dropped or
            pane.session.response_bytes_dropped > pane.acknowledged_response_dropped or
            pane.write_refusals > pane.acknowledged_write_refusals or
            pane.native_delivery_failures > pane.acknowledged_delivery_failures;
    }

    /// Mark every loss counted so far as seen; the counters stay cumulative.
    pub fn acknowledgeLoss(pane: *Pane) void {
        pane.acknowledged_outbound_dropped = pane.outbound_dropped;
        pane.acknowledged_response_dropped = pane.session.response_bytes_dropped;
        pane.acknowledged_write_refusals = pane.write_refusals;
        pane.acknowledged_delivery_failures = pane.native_delivery_failures;
    }

    pub fn acceptsInput(pane: *const Pane) bool {
        return pane.phase == .starting or pane.phase == .live;
    }

    /// The child's OSC 0/2 title, or "" (not reported; the caller picks a
    /// fallback). Valid until the next title report.
    pub fn title(pane: *const Pane) []const u8 {
        return pane.session.title();
    }

    /// The child's OSC 7 working directory as a plain absolute path (URL
    /// scheme and percent-encoding already decoded), or "" when unknown. Same
    /// lifetime and same empty-means-unreported rule as `title`.
    pub fn pwd(pane: *const Pane) []const u8 {
        return pane.session.pwd();
    }

    /// A BEL arrived and nobody has acknowledged it. Read-only: a hidden tab
    /// paints its attention marker from this, every frame, without consuming
    /// it. `clearBell` is the acknowledgement.
    pub fn bellRung(pane: *const Pane) bool {
        return pane.session.bell_rung;
    }

    /// Acknowledge the bell. The latch lives on the heap-owned session.
    pub fn clearBell(pane: *const Pane) void {
        pane.session.bell_rung = false;
    }

    /// Whether the cursor sits at a shell prompt rather than mid-output.
    /// False for every shell without OSC 133 integration — the honest
    /// unknown, not a guess.
    pub fn atPrompt(pane: *const Pane) bool {
        return pane.session.atPrompt();
    }

    /// A command ran and the shell is back at its prompt, unattended since:
    /// the idle half of attention. Read-only, like `bellRung`.
    pub fn promptReturned(pane: *const Pane) bool {
        return pane.session.prompt_returned;
    }

    /// Acknowledge the return to the prompt.
    pub fn clearPromptReturned(pane: *const Pane) void {
        pane.session.prompt_returned = false;
    }
};

/// Close frees a slot eagerly (no tombstone). Identity never repeats, so a
/// late event for a retired terminal resolves to no slot.
pub const RegistryState = enum { vacant, active };

pub fn replicaOwnerForPane(pane: *const Pane) ReplicaOwner {
    return provider_contract.localReplicaOwner(pane.id, pane.session_generation);
}

pub const LocalProvider = struct {
    context_id: u64,
    gpa: std.mem.Allocator,
    io: std.Io,
    /// ONE uniform array. The old split between an eager `terminals[2]` and
    /// an `overflow` tail encoded the two-pane workspace in the registry and
    /// forced every caller through an index-translating accessor.
    slots: [max_terminals]Pane = undefined,
    states: [max_terminals]RegistryState = @splat(.vacant),
    next_terminal_raw: u64 = first_terminal_raw,
    next_pty_key: u64 = 1,
    /// Configured `scrollback-limit` for lazily minted sessions.
    max_scrollback_bytes: usize = grid.Session.max_scrollback,
    /// The configured shell command shared by every pane's `argv`.
    shell_command: ShellCommand = .{},

    pub fn create(gpa: std.mem.Allocator, session: *grid.Session) !*LocalProvider {
        return createWithIo(gpa, std.Io.failing, session);
    }

    /// The argv every new pane spawns with: the configured `shell`/`command`
    /// when there is one, the built-in login shell otherwise.
    pub fn defaultArgv(provider: *const LocalProvider) []const []const u8 {
        if (provider.shell_command.isSet()) return provider.shell_command.argv();
        return shell_argv;
    }

    /// Adopt a configured shell at startup, re-pointing not-yet-spawned panes
    /// still on the built-in argv. False keeps the built-in shell.
    pub fn setShellCommand(provider: *LocalProvider, value: []const u8) bool {
        if (!provider.shell_command.set(value)) return false;
        const configured = provider.shell_command.argv();
        for (provider.slots[0..], provider.states[0..]) |*pane, state| {
            if (state == .vacant) continue;
            if (pane.argv.ptr == shell_argv.ptr) pane.argv = configured;
        }
        return true;
    }

    /// The window opens with exactly ONE terminal. Every further terminal is
    /// minted by `createTerminal`, which allocates its session then — the app
    /// no longer pre-allocates emulators for panes nobody asked for.
    pub fn createWithIo(gpa: std.mem.Allocator, io: std.Io, session: *grid.Session) !*LocalProvider {
        const provider = try gpa.create(LocalProvider);
        errdefer gpa.destroy(provider);
        provider.* = .{ .gpa = gpa, .io = io, .context_id = try @import("provider_contract").context.allocate() };
        provider.slots[0] = .{
            .id = initialTerminalRef(0),
            .session = session,
            .pty_key = ptyKey(0),
            .argv = shell_argv,
        };
        provider.states[0] = .active;
        provider.next_terminal_raw = first_terminal_raw + 1;
        provider.next_pty_key = 2;
        return provider;
    }

    pub fn destroy(provider: *LocalProvider) void {
        const gpa = provider.gpa;
        for (0..max_terminals) |index| {
            if (provider.states[index] == .active) provider.slots[index].session.destroy();
        }
        gpa.destroy(provider);
    }

    pub fn slot(provider: *LocalProvider, index: usize) *Pane {
        return &provider.slots[index];
    }

    pub fn slotConst(provider: *const LocalProvider, index: usize) *const Pane {
        return &provider.slots[index];
    }

    pub fn activeCount(provider: *const LocalProvider) usize {
        var count: usize = 0;
        for (provider.states) |state| if (state == .active) {
            count += 1;
        };
        return count;
    }

    /// Slots holding (or about to hold) a pty; ended or failed panes do not
    /// count against the SDK's pty table.
    pub fn liveShellCount(provider: *const LocalProvider) usize {
        var count: usize = 0;
        for (provider.states, 0..) |state, index| {
            if (state != .active) continue;
            if (provider.slots[index].acceptsInput()) count += 1;
        }
        return count;
    }

    pub fn slotIndex(provider: *const LocalProvider, terminal_ref: TerminalRef) ?usize {
        if (!provider_contract.isLocal(terminal_ref)) return null;
        for (0..max_terminals) |index| {
            if (provider.states[index] != .active) continue;
            if (provider.slots[index].id.eql(terminal_ref)) return index;
        }
        return null;
    }

    pub fn terminal(provider: *LocalProvider, terminal_ref: TerminalRef) ?*Pane {
        const index = provider.slotIndex(terminal_ref) orelse return null;
        return &provider.slots[index];
    }

    pub fn terminalConst(provider: *const LocalProvider, terminal_ref: TerminalRef) ?*const Pane {
        const index = provider.slotIndex(terminal_ref) orelse return null;
        return &provider.slots[index];
    }

    pub fn terminalRefs(provider: *const LocalProvider, out: []TerminalRef) usize {
        var count: usize = 0;
        for (0..max_terminals) |index| {
            if (count == out.len) break;
            if (provider.states[index] != .active) continue;
            out[count] = provider.slots[index].id;
            count += 1;
        }
        return count;
    }

    pub fn contains(provider: *const LocalProvider, terminal_ref: TerminalRef) bool {
        return provider.terminalConst(terminal_ref) != null;
    }

    pub fn owner(provider: *const LocalProvider, terminal_ref: TerminalRef) ?ReplicaOwner {
        const pane = provider.terminalConst(terminal_ref) orelse return null;
        return replicaOwnerForPane(pane);
    }

    pub fn ownerIsCurrent(provider: *const LocalProvider, owner_value: ReplicaOwner) bool {
        const current = provider.owner(owner_value.terminal_ref) orelse return false;
        return current.eql(owner_value);
    }

    pub fn terminalForPty(provider: *LocalProvider, key: u64) ?*Pane {
        for (0..max_terminals) |index| {
            if (provider.states[index] != .active) continue;
            if (provider.slots[index].pty_key == key) return &provider.slots[index];
        }
        return null;
    }

    pub fn createTerminal(provider: *LocalProvider) !*Pane {
        // Refuse past the shell ceiling rather than mint a dead pane.
        if (provider.liveShellCount() >= max_live_shells) return error.TerminalCapacityReached;
        if (provider.activeCount() >= max_terminals) return error.TerminalCapacityReached;
        if (provider.next_terminal_raw >= std.math.maxInt(u64) - 1 or provider.next_pty_key >= std.math.maxInt(u64) - 1) return error.TerminalIdentityExhausted;
        const next_ref = localRef(@enumFromInt(provider.next_terminal_raw));
        for (0..max_terminals) |occupied| {
            if (provider.states[occupied] != .active) continue;
            const existing = provider.slots[occupied];
            if (existing.id.eql(next_ref) or existing.pty_key == provider.next_pty_key) return error.TerminalIdentityCollision;
        }
        if (provider.next_pty_key == clipboard_key or provider.next_pty_key == paste_clipboard_key) return error.TerminalIdentityCollision;
        var index: usize = 0;
        while (index < max_terminals and provider.states[index] != .vacant) : (index += 1) {}
        if (index == max_terminals) return error.TerminalCapacityReached;
        // The session is allocated HERE, at the moment a terminal is asked
        // for, not eagerly at startup for panes that may never exist.
        const session = try grid.Session.createWithScrollback(provider.gpa, provider.io, 80, 24, provider.max_scrollback_bytes);
        errdefer session.destroy();
        provider.slots[index] = .{
            .id = next_ref,
            .session = session,
            .pty_key = provider.next_pty_key,
            .argv = provider.defaultArgv(),
        };
        provider.next_terminal_raw += 1;
        provider.next_pty_key += 1;
        provider.states[index] = .active;
        return &provider.slots[index];
    }

    /// Free a terminal's emulator and slot. The caller has already killed the
    /// pty and ended its effects.
    pub fn destroyTerminal(provider: *LocalProvider, id: TerminalRef) bool {
        const index = provider.slotIndex(id) orelse return false;
        provider.slots[index].session.destroy();
        provider.states[index] = .vacant;
        return true;
    }
};

pub const Provider = LocalProvider;
