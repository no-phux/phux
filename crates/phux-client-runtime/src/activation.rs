//! Wire activation enums lifted to their `phux-core` domain twins.
//!
//! The core types own the stable words (`as_str`) that `ls --json`, the
//! glance, and the bindings print; converting here keeps one table of
//! those words instead of one per surface.

use phux_core::activation::{ExecState as CoreExecState, SessionLifetime as CoreLifetime};
use phux_protocol::wire::frame::{ExecState, SessionLifetime};

/// The core execution state a wire [`ExecState`] names.
#[must_use]
pub const fn core_exec_state(state: ExecState) -> CoreExecState {
    match state {
        ExecState::Starting => CoreExecState::Starting,
        ExecState::Running => CoreExecState::Running,
        ExecState::Stopping => CoreExecState::Stopping,
        ExecState::Completed => CoreExecState::Completed,
        ExecState::Failed => CoreExecState::Failed,
        ExecState::Lost => CoreExecState::Lost,
        ExecState::Abandoned => CoreExecState::Abandoned,
    }
}

/// The core session lifetime a wire [`SessionLifetime`] names.
#[must_use]
pub const fn core_lifetime(lifetime: SessionLifetime) -> CoreLifetime {
    match lifetime {
        SessionLifetime::Persistent => CoreLifetime::Persistent,
        SessionLifetime::Ephemeral => CoreLifetime::Ephemeral,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_state_words_match_the_wire_spelling() {
        let states = [
            (ExecState::Starting, "starting"),
            (ExecState::Running, "running"),
            (ExecState::Stopping, "stopping"),
            (ExecState::Completed, "completed"),
            (ExecState::Failed, "failed"),
            (ExecState::Lost, "lost"),
            (ExecState::Abandoned, "abandoned"),
        ];
        for (state, word) in states {
            assert_eq!(core_exec_state(state).as_str(), word);
        }
    }

    #[test]
    fn lifetime_words_match_the_wire_spelling() {
        assert_eq!(
            core_lifetime(SessionLifetime::Persistent).as_str(),
            "persistent"
        );
        assert_eq!(
            core_lifetime(SessionLifetime::Ephemeral).as_str(),
            "ephemeral"
        );
    }
}
