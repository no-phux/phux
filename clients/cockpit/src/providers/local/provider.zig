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

/// The local registry ceiling. A tab owns a tree of at most
/// `layout.max_panes` panes and there can be many tabs, so the registry is
/// sized for the whole window rather than for one pane pair. 32 is far past
/// what a person keeps alive and still leaves the registry a flat, scannable
/// array.
pub const max_terminals: usize = 32;

/// How many local terminals can hold a LIVE SHELL at one time. This is a
/// different question from `max_terminals` above, which sizes the registry
/// array and the persistable topology.
///
/// The effects layer keeps ONE fixed pty table for the whole process
/// (`Effects.pty_slots`, sized by `native_sdk.max_effect_ptys`), so the
/// (N+1)-th `ptySpawn` is refused with a `.rejected` exit no matter how much
/// registry room is left. A pane minted past that ceiling is born DEAD: its
/// emulator never receives a byte, so it paints an empty grid with a cursor at
/// the origin and nothing else, forever. That is the whole of
/// phux-cockpit-pg1 — five tabs, a blank fifth pane, `dispatch_errors=0`,
/// because a refused spawn is a normal exit event and not an error.
///
/// DERIVED, never restated as a literal: the ceiling belongs to the pinned
/// SDK, and a second copy of it here would go stale the first time the pin
/// moves and hand the symptom straight back.
///
/// phux-cockpit-ipg raised the SDK's table from 4 to 32 (see
/// `docs/sdk-patches/`), which is what makes this an upper bound rather than
/// THE bound: at 32 it equals `max_terminals` above, and the ceiling a person
/// actually reaches is `topology.max_tabs` (16) or `layout.max_panes` (16) —
/// both of them Cockpit's own, both of them nameable. Re-measure against the
/// shipped bundle with:
///
///   ./scripts/drive-shell-ceiling.sh --want 8 --measure
///
/// which drives the real app and reports what it reached, plus the rss,
/// thread and descriptor cost of every shell it opened.
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

/// Every local terminal spawns the SAME login shell. There was never a
/// per-slot argv distinction — the old `terminal_1_argv`/`terminal_2_argv`
/// pair held identical text — and a registry of 32 uniform slots cannot
/// carry a per-index table anyway.
const shell_argv: []const []const u8 = if (builtin.os.tag == .macos)
    &.{ "/bin/zsh", "-l", "-c", "cd \"$HOME\" && exec /bin/zsh -i" }
else
    default_shell_argv;

/// Storage and argv for a configured `shell` / `command`.
///
/// The value is treated as a COMMAND LINE run by the login shell, which is
/// what `command` means in the terminal these users arrive from: `shell =
/// /opt/homebrew/bin/fish` and `command = tmux attach` both have to work, and
/// single-quoting the whole value the way `paneArgvIn` quotes a DIRECTORY
/// would turn the second into one nonexistent program named "tmux attach".
///
/// That asymmetry is deliberate rather than an oversight. `paneArgvIn` quotes
/// because a working directory arrives from OSC 7 — a remote, hostile-capable
/// source — and must never become a command. This value arrives from the
/// user's own config file, which already names the program that is about to
/// run as them; quoting it would buy no safety and cost the feature.
///
/// `exec` so the configured program REPLACES the wrapper shell rather than
/// running underneath it, which keeps the pty's process the one the user asked
/// for and makes exit behave.
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

    /// Adopt `value`, or leave the built-in shell in place when it is empty,
    /// carries a NUL (which the SDK rejects and the C boundary would truncate),
    /// or does not fit. Every rejection degrades to the default shell rather
    /// than to a pane that cannot open.
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

/// Byte ceiling for the generated `cd ... ; exec ...` command word.
///
/// The SDK caps a spawn's argv at `max_effect_argv_bytes` (2048) across ALL
/// arguments and refuses the whole spawn past it, so the command must stay
/// well inside that with the shell path and flags accounted for. 1024 leaves
/// better than 2x headroom and still holds any real path: single-quoting only
/// grows a path when it contains quotes, which directories essentially never
/// do at length.
pub const max_cwd_command_bytes: usize = 1024;

/// Storage for one generated cwd-carrying argv. The caller owns it and must
/// keep it alive as long as the argv is: `Pane.argv` is a slice, and the SDK
/// copies argv at `ptySpawn`, so this only has to outlive the spawn call —
/// but a `Pane` that holds the argv holds the storage with it.
pub const CwdArgv = struct {
    command: [max_cwd_command_bytes]u8 = undefined,
    slots: [4][]const u8 = undefined,
};

/// The argv for a shell that STARTS in `cwd` — the working-directory
/// inheritance a new terminal or split wants from the pane it was opened
/// from.
///
/// This has to ride argv because there is nowhere else to put it: the SDK's
/// `PtySpawnOptions` carries key/argv/cols/rows/term/on_event and nothing
/// else, and its child environment is the bound host environ verbatim
/// (`flattenPtyEnviron`) — no cwd field, no env field, no hook.
///
/// The path is SINGLE-quoted with embedded quotes escaped as `'\''`, which is
/// the only POSIX quoting with no escape sequences of its own: `$`, backtick,
/// `"`, `\`, spaces, newlines, and `;` are all literal inside it, so a
/// hostile directory name cannot break out of the word and become a command.
///
/// Failure to `cd` falls back to `$HOME` rather than aborting: `&&` would
/// leave the shell exiting immediately (no exec, no shell, a pane that dies
/// on open) whenever the directory moved, was unmounted, or came from a
/// remote OSC 7. A terminal in the wrong directory beats no terminal.
///
/// Returns the plain no-cwd argv when `cwd` is empty, not absolute, contains
/// a NUL (which the SDK rejects and the C boundary would truncate), or does
/// not fit — the degradation is exactly "the first terminal's behavior".
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

    // `-l` only where the plain argv already asks for a login shell (macOS,
    // where the user's PATH comes from the login environment). Elsewhere the
    // default argv is a bare interactive `/bin/sh`, and `sh -l` is not
    // portable enough to introduce here.
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
    /// Watermarks for the loss counters above: how much loss the operator has
    /// already been shown.
    ///
    /// The counters themselves are CUMULATIVE and must stay that way — they
    /// are the evidence in each surface's accessibility label, and a diagnostic
    /// that resets is a diagnostic that lies. But attention is a different
    /// question from evidence: "this terminal has ever dropped a byte" is true
    /// forever after the first drop, and deriving an attention signal straight
    /// from it latched the tab band permanently open for the life of the
    /// session. Attention asks "has anything happened SINCE you looked", which
    /// is what these watermarks answer.
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

    /// Mark everything counted so far as seen. The pane is a handle onto a
    /// heap-owned session, so this takes a `*Pane` for its own fields and
    /// reaches through for the session's — the same split `clearBell` has.
    pub fn acknowledgeLoss(pane: *Pane) void {
        pane.acknowledged_outbound_dropped = pane.outbound_dropped;
        pane.acknowledged_response_dropped = pane.session.response_bytes_dropped;
        pane.acknowledged_write_refusals = pane.write_refusals;
        pane.acknowledged_delivery_failures = pane.native_delivery_failures;
    }

    pub fn acceptsInput(pane: *const Pane) bool {
        return pane.phase == .starting or pane.phase == .live;
    }

    /// The child's OSC 0/2 title, or "" when it never reported one. The slice
    /// belongs to the pane's session and stays valid until the next title
    /// report. EMPTY IS NOT A TITLE — it means "not reported", and the caller
    /// owns the fallback (a slot label, the pwd's basename, whatever the
    /// chrome wants), because only the caller knows what it is rendering.
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

    /// Acknowledge the bell. Takes a `*const Pane` because the latch lives on
    /// the heap-owned session, not in the pane's own bytes — the pane is a
    /// handle here, and the view holding a const one is still allowed to say
    /// "the user has seen it".
    pub fn clearBell(pane: *const Pane) void {
        pane.session.bell_rung = false;
    }

    /// Whether the cursor sits at a shell prompt rather than mid-output.
    /// False for every shell without OSC 133 integration — the honest
    /// unknown, not a guess.
    pub fn atPrompt(pane: *const Pane) bool {
        return pane.session.atPrompt();
    }
};

/// A slot is either free or holds a live terminal. There is no `.closing`
/// tombstone: close destroys the session eagerly and frees the slot, because
/// a tombstone waiting on a pty exit that may never arrive held both a
/// registry slot and a whole emulator hostage. Identity never repeats
/// (`next_terminal_raw` and `next_pty_key` only move forward), so a late
/// event for a retired terminal resolves to no slot and is ignored.
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
    /// Scrollback ceiling handed to every session minted from here. Carried on
    /// the provider because terminals are created LAZILY, long after the
    /// config was read — there is nowhere else for a user's `scrollback-limit`
    /// to wait. Defaults to the session default so a provider built without
    /// config behaves exactly as before.
    max_scrollback_bytes: usize = grid.Session.max_scrollback,
    /// A user-configured `shell` / `command`, built ONCE into provider-owned
    /// storage. Null means the built-in login shell. One shell serves every
    /// pane — there has never been a per-slot argv distinction — so a single
    /// buffer on the provider outlives every `Pane.argv` slice into it.
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

    /// Adopt a configured shell. Called once at startup, AFTER the first pane
    /// exists (the config cannot be read before the model is built), so it
    /// also re-points panes that are still carrying the built-in argv. Nothing
    /// has spawned yet at that point — `Pane.argv` is read at `ptySpawn` — so
    /// this is a fix-up, not a mutation of a running shell.
    ///
    /// False means the value was rejected and the built-in shell stands.
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

    pub fn createSingleWithIo(gpa: std.mem.Allocator, io: std.Io, session: *grid.Session) !*LocalProvider {
        return createWithIo(gpa, io, session);
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

    /// Occupancy IS the active count now that closing frees eagerly. Kept as
    /// its own name because callers ask two different questions of it.
    pub fn occupiedCount(provider: *const LocalProvider) usize {
        return provider.activeCount();
    }

    /// How many registry slots are holding — or are about to hold — a pty of
    /// their own, which is what the effects layer's fixed pty table actually
    /// counts. NOT `activeCount`: a `.failed` or `.ended` pane still owns a
    /// registry slot but owns no pty, so counting those would wedge the app at
    /// its ceiling with dead husks and refuse a shell there was room for.
    ///
    /// `acceptsInput` is exactly the right predicate and is reused rather than
    /// re-spelled — a pane takes typed bytes precisely when a child is there
    /// to read them.
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
        // The SHELL ceiling is checked first and refuses hardest, because it
        // is the one a person actually reaches. A pane minted past it gets a
        // `.rejected` spawn and an empty grid nobody can type into, so the
        // honest answer is to refuse the pane here — where every caller
        // already handles a refusal — rather than to hand back a husk that
        // looks like a working tab until you click it. See `max_live_shells`.
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

    /// Retire a terminal for good: the emulator is freed immediately and the
    /// slot returns to the pool. The caller is responsible for having already
    /// killed the pty and ended any effect keyed to this terminal — nothing
    /// here can be reached through the freed session afterwards.
    pub fn destroyTerminal(provider: *LocalProvider, id: TerminalRef) bool {
        const index = provider.slotIndex(id) orelse return false;
        provider.slots[index].session.destroy();
        provider.states[index] = .vacant;
        return true;
    }
};

pub const Provider = LocalProvider;
