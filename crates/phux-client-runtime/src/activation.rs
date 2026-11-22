//! Wire activation enums lifted to their `phux-core` domain twins.
//!
//! The core types own the stable words (`as_str`) that `ls --json`, the
//! glance, and the bindings print; converting here keeps one table of
//! those words instead of one per surface.

use phux_core::activation::{ExecState as CoreExecState, SessionLifetime as CoreLifetime};
use phux_protocol::wire::frame::{ExecState, SessionLifetime};

/// The core execution state a wire [`ExecState`] names; `None` for a newer
/// server's state this build does not know.
#[must_use]
pub const fn core_exec_state(state: ExecState) -> Option<CoreExecState> {
    Some(match state {
        ExecState::Starting => CoreExecState::Starting,
        ExecState::Running => CoreExecState::Running,
        ExecState::Stopping => CoreExecState::Stopping,
        ExecState::Completed => CoreExecState::Completed,
        ExecState::Failed => CoreExecState::Failed,
        ExecState::Lost => CoreExecState::Lost,
        ExecState::Abandoned => CoreExecState::Abandoned,
        ExecState::Unknown(_) => return None,
    })
}

/// The core session lifetime a wire [`SessionLifetime`] names; `None` for a
/// newer server's lifetime this build does not know.
#[must_use]
pub const fn core_lifetime(lifetime: SessionLifetime) -> Option<CoreLifetime> {
    Some(match lifetime {
        SessionLifetime::Persistent => CoreLifetime::Persistent,
        SessionLifetime::Ephemeral => CoreLifetime::Ephemeral,
        SessionLifetime::Unknown(_) => return None,
    })
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
            assert_eq!(
                core_exec_state(state).map(CoreExecState::as_str),
                Some(word)
            );
        }
    }

    #[test]
    fn lifetime_words_match_the_wire_spelling() {
        let word = |lifetime| core_lifetime(lifetime).map(CoreLifetime::as_str);
        assert_eq!(word(SessionLifetime::Persistent), Some("persistent"));
        assert_eq!(word(SessionLifetime::Ephemeral), Some("ephemeral"));
        assert_eq!(word(SessionLifetime::Unknown(9)), None);
    }
}
