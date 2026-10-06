//! Cheap observations for native transitions whose projected state can change
//! without a command. Terminal bytes and measured geometry are not observed.
const Model = @import("../model.zig").Model;
const TerminalRef = @import("../phux_support.zig").TerminalRef;

pub const Domains = struct {
    focus: bool = false,
    persistence: bool = false,
    /// A paste was held for confirmation, or its answer let it go.
    paste: bool = false,

    pub fn needsSnapshot(self: Domains) bool {
        return self.focus or self.persistence or self.paste;
    }
};

pub const Checkpoint = struct {
    sequence: u64,
    revision: u64,
    window: usize,
    terminal: ?TerminalRef,
    persistence_failed: bool,
    paste_pending: bool,

    pub fn capture(model: *const Model, sequence: u64, revision: u64) Checkpoint {
        return .{
            .sequence = sequence,
            .revision = revision,
            .window = model.active_window,
            .terminal = model.focusedTerminalRef(),
            .persistence_failed = model.state.write_failed,
            .paste_pending = model.paste_pending != null,
        };
    }

    pub fn changes(self: Checkpoint, model: *const Model) Domains {
        const next = capture(model, self.sequence, self.revision);
        return .{
            .focus = self.focusChanged(next),
            .persistence = self.persistence_failed != next.persistence_failed,
            .paste = self.paste_pending != next.paste_pending,
        };
    }

    fn focusChanged(self: Checkpoint, next: Checkpoint) bool {
        if (self.window != next.window) return true;
        const before = self.terminal orelse return next.terminal != null;
        const after = next.terminal orelse return true;
        return !before.eql(after);
    }
};
