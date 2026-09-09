//! Cheap observations for native transitions whose projected state can change
//! without a command. Terminal bytes and measured geometry are not observed.
const Model = @import("../model.zig").Model;
const TerminalRef = @import("../phux_support.zig").TerminalRef;

pub const Domains = struct {
    focus: bool = false,
    persistence: bool = false,

    pub fn needsSnapshot(self: Domains) bool {
        return self.focus or self.persistence;
    }
};

pub const Checkpoint = struct {
    sequence: u64,
    revision: u64,
    window: usize,
    terminal: ?TerminalRef,
    persistence_failed: bool,

    pub fn capture(model: *const Model, sequence: u64, revision: u64) Checkpoint {
        return .{
            .sequence = sequence,
            .revision = revision,
            .window = model.active_window,
            .terminal = model.focusedTerminalRef(),
            .persistence_failed = model.state.write_failed,
        };
    }

    pub fn changes(self: Checkpoint, model: *const Model) Domains {
        const next = capture(model, self.sequence, self.revision);
        return .{
            .focus = self.focusChanged(next),
            .persistence = self.persistence_failed != next.persistence_failed,
        };
    }

    fn focusChanged(self: Checkpoint, next: Checkpoint) bool {
        if (self.window != next.window) return true;
        const before = self.terminal orelse return next.terminal != null;
        const after = next.terminal orelse return true;
        return !before.eql(after);
    }
};
