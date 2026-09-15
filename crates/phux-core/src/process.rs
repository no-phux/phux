//! Typed terminal process facts — the `process` object of the
//! `GET_TERMINAL_STATE` JSON (L1 §6.3) and the exit outcome a Terminal's
//! engine reports when its child leaves.
//!
//! Like [`crate::screen::ScreenState`], these types live in `phux-core` so
//! the server that *produces* them and any consumer that *deserializes*
//! them share one definition. They are pure data: every fact is sourced by
//! the server from the kernel (the PTY's own child, the tty's foreground
//! process group) or from the pane's OSC-133 marks, and a fact the server
//! could not obtain is `None` (JSON `null`) — never a guess.
//!
//! Privacy boundary: only the PTY's own child and the tty's foreground
//! process group are ever queried, and only the foreground's argv0
//! basename leaves the server — never the argv tail, the environment, or
//! any unrelated pid (the same boundary as `phux.pane-occupant/v1`).

use serde::{Deserialize, Serialize};

/// `schema_version` stamped on the `GET_TERMINAL_STATE` JSON.
///
/// `1` is the first versioned shape: it adds the `process` object
/// ([`TerminalProcessState`]) and populates `shell_state`. The version
/// moves only when a key is removed, renamed, or retyped; added keys are
/// ignored by older consumers.
pub const TERMINAL_STATE_SCHEMA_VERSION: u32 = 1;

/// How a Terminal's child process left, as the kernel reported it.
///
/// Exactly one of the two fields is `Some` for a reaped child: `status` for
/// an `_exit(n)`, `signal` for a death by signal. Both are `None` when the
/// cause is unknown (the child could not be reaped in the EOF budget, or
/// the adopted-child path could not wait on it). `RESOURCE_CLOSED` carries
/// `status` in `exit_status` and `signal` in its additive field 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExitOutcome {
    /// The code passed to `_exit(n)`, when the child exited normally.
    pub status: Option<i32>,
    /// The terminating signal number, when the child died by a signal.
    pub signal: Option<i32>,
}

impl ExitOutcome {
    /// Neither a code nor a signal is known.
    pub const UNKNOWN: Self = Self {
        status: None,
        signal: None,
    };

    /// A normal exit with `code`.
    #[must_use]
    pub const fn exited(code: i32) -> Self {
        Self {
            status: Some(code),
            signal: None,
        }
    }

    /// A death by signal number `signal`.
    #[must_use]
    pub const fn signaled(signal: i32) -> Self {
        Self {
            status: None,
            signal: Some(signal),
        }
    }
}

/// A process identity that survives pid reuse: the pid (or pgid) paired
/// with the process's kernel start time.
///
/// Two readings name the same process exactly when both fields are equal.
/// A recycled pid carries a different `start_ms`, so a consumer that stored
/// `(pid, start_ms)` can tell the process it saw from a later one wearing
/// the same number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    /// The process id.
    pub pid: i32,
    /// Process start time in Unix milliseconds, or `None` when the kernel
    /// query failed. On Linux it derives from `/proc/<pid>/stat` field 22
    /// plus the boot time, so it carries up to one second of absolute
    /// error. The server reads the boot time once per server process, so
    /// one process reports the same value on every query. The kernel
    /// recomputes its boot time on a wall-clock step, so a server started
    /// after a step can report a different value for a process that
    /// predates it; compare generations within one server's lifetime.
    #[serde(default)]
    pub start_ms: Option<u64>,
}

/// The process group that owns the pane's tty right now — what the user is
/// interacting with, which is not necessarily the shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundProcess {
    /// Foreground process group id (`tcgetpgrp` on the PTY master).
    pub pgid: i32,
    /// Start time of the group leader in Unix milliseconds, or `None`.
    #[serde(default)]
    pub start_ms: Option<u64>,
    /// The group leader's argv0 basename with a login-shell dash stripped
    /// (`-zsh` reads `zsh`). `None` when the argv query failed.
    #[serde(default)]
    pub name: Option<String>,
}

/// The pane's shell-integration state, derived from OSC-133 marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptState {
    /// No OSC-133 mark has been seen yet (or the shell has no integration).
    #[default]
    Unknown,
    /// The shell is showing a prompt or reading input (`A`, `B`, or `D`).
    AtPrompt,
    /// A command is executing (`C`).
    Running,
}

/// The prompt facet: the current [`PromptState`] and the exit code the
/// most recent `OSC 133 ; D` mark reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PromptFacet {
    /// Where the shell is in its prompt/command cycle.
    pub state: PromptState,
    /// Exit code from the last `D` mark, or `None` when no `D` carried one.
    #[serde(default)]
    pub last_exit_code: Option<i32>,
}

/// [`ProcessExit::reason`] for an exit the Terminal engine observed as PTY
/// EOF. The vocabulary is `RESOURCE_CLOSED`'s close reasons in snake case;
/// consumers MUST tolerate an unknown value.
pub const EXIT_REASON_EXITED: &str = "exited";

/// The exit facet: how the child left and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessExit {
    /// `_exit(n)` code, when the child exited normally.
    #[serde(default)]
    pub status: Option<i32>,
    /// Terminating signal number, when the child died by a signal.
    #[serde(default)]
    pub signal: Option<i32>,
    /// Why the resource is leaving, in the `RESOURCE_CLOSED` close-reason
    /// vocabulary (see [`EXIT_REASON_EXITED`]). A string, not an enum, so a
    /// newer server's reason never fails an older consumer's parse.
    pub reason: String,
    /// When the exit was observed, in Unix milliseconds.
    #[serde(default)]
    pub exited_at_ms: Option<u64>,
}

impl ProcessExit {
    /// An exit observed as PTY EOF with `outcome`, at `exited_at_ms`.
    #[must_use]
    pub fn observed(outcome: ExitOutcome, exited_at_ms: Option<u64>) -> Self {
        Self {
            status: outcome.status,
            signal: outcome.signal,
            reason: EXIT_REASON_EXITED.to_owned(),
            exited_at_ms,
        }
    }
}

/// The `process` object of the `GET_TERMINAL_STATE` JSON.
///
/// Every field is always present in the JSON; an unobtainable fact is
/// `null`. `foreground` and `cwd` are live kernel queries and are `null`
/// once the child has exited; `child` keeps naming the process that ran.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TerminalProcessState {
    /// The PTY's own child (usually the shell), with its start time.
    #[serde(default)]
    pub child: Option<ProcessIdentity>,
    /// The tty's foreground process group.
    #[serde(default)]
    pub foreground: Option<ForegroundProcess>,
    /// The child's current working directory, from the kernel.
    #[serde(default)]
    pub cwd: Option<String>,
    /// OSC-133 prompt state.
    #[serde(default)]
    pub prompt: PromptFacet,
    /// How the child left, once it has.
    #[serde(default)]
    pub exit: Option<ProcessExit>,
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    /// Unobtainable facts serialize as explicit `null`, never as a missing
    /// key, so a consumer can tell "no answer" from "older producer".
    #[test]
    fn empty_state_serializes_every_key_as_null() {
        let json = serde_json::to_value(TerminalProcessState::default()).expect("serialize");
        for key in ["child", "foreground", "cwd", "exit"] {
            assert!(json[key].is_null(), "{key} must be null, got {json}");
        }
        assert_eq!(json["prompt"]["state"], "unknown");
        assert!(json["prompt"]["last_exit_code"].is_null());
    }

    #[test]
    fn full_state_round_trips() {
        let state = TerminalProcessState {
            child: Some(ProcessIdentity {
                pid: 42,
                start_ms: Some(1_700_000_000_000),
            }),
            foreground: Some(ForegroundProcess {
                pgid: 43,
                start_ms: None,
                name: Some("vim".to_owned()),
            }),
            cwd: Some("/repo".to_owned()),
            prompt: PromptFacet {
                state: PromptState::AtPrompt,
                last_exit_code: Some(3),
            },
            exit: Some(ProcessExit::observed(ExitOutcome::signaled(9), Some(7))),
        };
        let json = serde_json::to_string(&state).expect("serialize");
        assert!(json.contains("\"state\":\"at_prompt\""), "got {json}");
        assert!(json.contains("\"reason\":\"exited\""), "got {json}");
        let back: TerminalProcessState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, state);
    }

    #[test]
    fn exit_outcome_constructors_set_exactly_one_field() {
        assert_eq!(ExitOutcome::exited(1).signal, None);
        assert_eq!(ExitOutcome::signaled(15).status, None);
        assert_eq!(ExitOutcome::default(), ExitOutcome::UNKNOWN);
    }
}
