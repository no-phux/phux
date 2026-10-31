//! Which agent binary is running in a pane (ADR-0046 §A), from the kernel's
//! foreground process group rather than the spoofable title.
//!
//! Agents ship as native binaries or scripts under a runtime, so wrappers
//! are unwrapped and matching is two-tiered ([`foreground_occupancy`]).
//! [`Occupancy`] is three-valued: "asked and nothing matched" is evidence,
//! "could not ask" is not. An [`Occupant`] pairs the pgid with the leader's
//! start time, queried only for agents, so a recycled pgid is a new
//! occupant; an unreadable start time is never evidence of change.

use super::rules::RuleSet;
use phux_resource::proc_query;

/// Interpreters that host an agent; the agent's name is later in argv.
const RUNTIME_WRAPPERS: [&str; 14] = [
    "node", "nodejs", "bun", "deno", "python", "python3", "sh", "bash", "zsh", "fish", "env",
    "npx", "uv", "uvx",
];
const PANE_SHELLS: [&str; 4] = ["sh", "bash", "zsh", "fish"];

/// Suffixes stripped from a script name before matching (`cli.js` -> `cli`).
const SCRIPT_SUFFIXES: [&str; 5] = [".js", ".mjs", ".cjs", ".py", ".ts"];

/// A foreground occupant: pgid plus leader start time. The pgid alone is a
/// recycled integer, so the pair is the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Occupant {
    /// The process group we successfully looked at.
    pub(crate) pgid: i32,
    /// Leader start time in a platform unit, compared only for equality;
    /// `None` when the query failed or is unsupported.
    pub(crate) started: Option<u64>,
}

impl Occupant {
    /// Build an occupant (tests state explicit pairs).
    pub(crate) const fn new(pgid: i32, started: Option<u64>) -> Self {
        Self { pgid, started }
    }

    /// Whether two readings are the same occupant: different pgids never
    /// are; equal pgids are unless both start times are known and differ.
    /// An unknown start time is not evidence of a restart.
    pub(crate) const fn same(self, other: Self) -> bool {
        if self.pgid != other.pgid {
            return false;
        }
        match (self.started, other.started) {
            (Some(mine), Some(theirs)) => mine == theirs,
            _ => true,
        }
    }
}

/// Who owns a pane's foreground process group, as far as the kernel said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Occupancy {
    /// Not answered (no fd, or the pgid/argv read failed). Not evidence;
    /// callers hold their beliefs.
    Unresolved,
    /// The foreground runs something matching no manifest: positive evidence
    /// that no known agent is there.
    Vacant {
        /// The process group looked at (not compared, so no start time).
        pgid: i32,
    },
    /// Resolved: an agent of `kind` owns the foreground pgid.
    Agent {
        /// Open-vocabulary kind slug, e.g. `"claude"`.
        kind: String,
        /// Who runs it (start-time paired).
        occupant: Occupant,
    },
}

/// Privacy-bounded description of the pane's foreground process.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PaneOccupant {
    /// Login-dash-stripped basename of `argv[0]`.
    pub(crate) foreground: String,
    /// The foreground is the pane's original child and a known shell.
    pub(crate) is_pane_shell: bool,
}

impl Occupancy {
    /// The pgid this answer is about; `None` when unresolved (the cheap
    /// probe compares it).
    pub(crate) const fn pgid(&self) -> Option<i32> {
        match self {
            Self::Unresolved => None,
            Self::Vacant { pgid } => Some(*pgid),
            Self::Agent { occupant, .. } => Some(occupant.pgid),
        }
    }
}

/// The cheap half of [`foreground_occupancy`]: `tcgetpgrp` only, polled
/// every tick so the argv read happens only when the pgid moved.
pub(crate) fn foreground_pgid(master_fd: Option<i32>) -> Option<i32> {
    proc_query::foreground_pgid(master_fd?)
}

/// Who occupies the foreground of this PTY: `tcgetpgrp`, then that
/// process's argv, then two-tier matching.
///
/// Tier 1 matches the basename of `argv[0]` (script suffix stripped), and
/// of every later argument behind a [runtime wrapper](RUNTIME_WRAPPERS).
/// Tier 2 matches path components of unambiguous program paths (`argv[0]`
/// with a `/`, or a wrapper argument with a script suffix), catching
/// `node .../claude-code/cli.js` and version-named installs. Plain string
/// arguments are never split, so `sh -c 'cd ~/.claude/foo'` is not an
/// agent.
#[cfg(test)]
pub(crate) fn foreground_occupancy(master_fd: Option<i32>, rules: &RuleSet) -> Occupancy {
    foreground_observation(master_fd, None, rules).0
}

/// Resolve agent identity and pane-shell availability from one process query.
pub(crate) fn foreground_observation(
    master_fd: Option<i32>,
    pane_child_pid: Option<i32>,
    rules: &RuleSet,
) -> (Occupancy, Option<PaneOccupant>) {
    let Some(fd) = master_fd else {
        return (Occupancy::Unresolved, None);
    };
    let Some(pgid) = proc_query::foreground_pgid(fd) else {
        return (Occupancy::Unresolved, None);
    };
    let Some(argv) = proc_query::process_argv(pgid) else {
        return (Occupancy::Unresolved, None);
    };
    let pane_occupant = pane_occupant(pgid, pane_child_pid, &argv);
    let occupancy = kind_from_argv(&argv, rules).map_or(Occupancy::Vacant { pgid }, |kind| {
        Occupancy::Agent {
            kind,
            // Only agents pay for the start-time query.
            occupant: Occupant::new(pgid, phux_resource::proc_query::process_start_time(pgid)),
        }
    });
    (occupancy, Some(pane_occupant))
}

fn pane_occupant(pgid: i32, pane_child_pid: Option<i32>, argv: &[String]) -> PaneOccupant {
    let foreground = argv.first().map_or("", String::as_str);
    let foreground = basename(foreground).trim_start_matches('-').to_owned();
    PaneOccupant {
        is_pane_shell: pane_child_pid == Some(pgid) && PANE_SHELLS.contains(&foreground.as_str()),
        foreground,
    }
}

/// The pure rule-matching core of [`foreground_occupancy`].
pub(crate) fn kind_from_argv(argv: &[String], rules: &RuleSet) -> Option<String> {
    let first = argv.first()?;

    if let Some(kind) = match_program(first, rules) {
        return Some(kind);
    }

    // `argv[0]` is only a host: keep looking.
    if !is_runtime_wrapper(first) {
        return None;
    }
    for arg in argv.iter().skip(1) {
        // Flags never name the program.
        if arg.starts_with('-') {
            continue;
        }
        if let Some(kind) = match_program(arg, rules) {
            return Some(kind);
        }
    }
    None
}

/// Match one program-shaped argument: tier 1 on its basename, tier 2 on its
/// path components when it is a program path.
fn match_program(arg: &str, rules: &RuleSet) -> Option<String> {
    let base = strip_script_suffix(basename(arg));
    if let Some(kind) = rules.kind_for_binary(base) {
        return Some(kind.to_owned());
    }
    if !is_program_path(arg) {
        return None;
    }
    arg.split('/')
        .filter(|part| !part.is_empty())
        .find_map(|part| rules.kind_for_binary(strip_script_suffix(part)))
        .map(str::to_owned)
}

/// Whether `arg` is a program path: contains `/`, no whitespace.
fn is_program_path(arg: &str) -> bool {
    arg.contains('/') && !arg.chars().any(char::is_whitespace)
}

/// The trailing path component of `arg`.
fn basename(arg: &str) -> &str {
    arg.rsplit('/').next().unwrap_or(arg)
}

/// Strip a known script suffix, if present.
fn strip_script_suffix(name: &str) -> &str {
    for suffix in SCRIPT_SUFFIXES {
        if let Some(stem) = name.strip_suffix(suffix) {
            return stem;
        }
    }
    name
}

/// Whether `arg` names an interpreter rather than an agent.
fn is_runtime_wrapper(arg: &str) -> bool {
    // A login shell arrives as `-zsh`.
    let name = basename(arg).trim_start_matches('-');
    RUNTIME_WRAPPERS.contains(&name)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
