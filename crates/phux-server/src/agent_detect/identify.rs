//! Which agent binary is running in a pane (ADR-0046 §A).
//!
//! Identity comes from the kernel, not from the title. The title is a
//! string the program chose to print — a shell that `echo`s "claude" would
//! fool it. The foreground process group of the pane's own PTY is what the
//! user is actually typing at.
//!
//! The wrinkle is that agent CLIs ship in two shapes: a native binary
//! (`argv[0] = "claude"`) and a script under a runtime (`node
//! .../@anthropic-ai/claude-code/cli.js`). So we unwrap runtime wrappers,
//! and we match on two tiers — see [`foreground_occupancy`].
//!
//! # Occupancy is three-valued, deliberately
//!
//! "I asked the kernel and nothing matched" and "I could not ask the kernel"
//! are different facts, and collapsing them into one `None` is what makes a
//! transient `sysctl` failure indistinguishable from a dead agent. Only the
//! first is *evidence*; the second is the absence of evidence. The detector
//! retracts on the first and holds on the second, so [`Occupancy`] keeps them
//! apart at the seam rather than in a comment.
//!
//! # An occupant is a pgid AND a start time
//!
//! A process group id is a small integer the kernel recycles. Two different
//! agents in one pane can therefore wear the same pgid, and an occupant
//! identity built on the pgid alone reads the second as the first — the same
//! class of blindness that discarding the pgid entirely used to cause, one
//! layer down. So [`Occupant`] pairs the pgid with the group leader's START
//! TIME, which the kernel never reissues for a recycled id: `(pgid, started)`
//! is unique for as long as anyone cares.
//!
//! The start time is queried only when the pgid resolves to an AGENT.
//! Vacancy is not compared to anything (`apply_vacancy` never reads the pgid),
//! so the overwhelmingly common pane — a shell — pays nothing for this.
//!
//! Like every other query here it is best-effort: `started: None` means the
//! platform or the kernel declined to answer, and an absent answer is never
//! evidence of a change ([`Occupant::same`]).

use super::rules::RuleSet;
use crate::proc_query;

/// Interpreters that merely *host* an agent: the interesting name is
/// further along argv, not at `argv[0]`.
const RUNTIME_WRAPPERS: [&str; 14] = [
    "node", "nodejs", "bun", "deno", "python", "python3", "sh", "bash", "zsh", "fish", "env",
    "npx", "uv", "uvx",
];
const PANE_SHELLS: [&str; 4] = ["sh", "bash", "zsh", "fish"];

/// Suffixes stripped from a script name before matching (`cli.js` -> `cli`).
const SCRIPT_SUFFIXES: [&str; 5] = [".js", ".mjs", ".cjs", ".py", ".ts"];

/// The identity of whatever holds a pane's foreground: a process group id
/// paired with the start time of its leader.
///
/// The pair, not the pgid, is the identity. A bare pgid is a recycled small
/// integer: the kernel is free to hand `claude`'s old pgid to the `codex` the
/// user starts next, and a detector comparing pgids alone would call that the
/// same occupant and never notice the swap. Pid wraparound makes the window
/// narrow, not absent, and "narrow" is not a property a correctness argument
/// can rest on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Occupant {
    /// The process group we successfully looked at.
    pub(crate) pgid: i32,
    /// The group leader's start time, in whatever monotonic-per-boot unit the
    /// platform reports (macOS: microseconds since the epoch; Linux: clock
    /// ticks since boot). Compared for EQUALITY only, never ordered or
    /// converted, so the unit never has to be reconciled across platforms.
    ///
    /// `None` when the platform does not answer the question or the query
    /// failed — the same "absence of evidence" the enclosing [`Occupancy`]
    /// keeps apart from evidence, one field down.
    pub(crate) started: Option<u64>,
}

impl Occupant {
    /// Build an occupant. Public for the tests that need to state a specific
    /// `(pgid, started)` pair without a live kernel.
    pub(crate) const fn new(pgid: i32, started: Option<u64>) -> Self {
        Self { pgid, started }
    }

    /// Whether `self` and `other` are the SAME occupant.
    ///
    /// Different pgids are always different occupants. Equal pgids are the
    /// same occupant unless both sides carry a start time and the two
    /// disagree — a recycled id.
    ///
    /// The asymmetry is deliberate and is the module's rule applied one level
    /// down: a start time we could not read is not evidence of a restart, and
    /// manufacturing one out of a failed query would turn a `sysctl` blip into
    /// a metadata write on every pane (ADR-0046 decision 7).
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

/// Who owns a pane's foreground process group, as far as the kernel would
/// tell us.
///
/// The three values are not degrees of confidence, they are different
/// *questions answered*: [`Self::Unresolved`] means the question was not
/// answered at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Occupancy {
    /// Could not be answered: no master fd, or the pgid / argv read failed.
    /// NOT evidence of anything. Callers MUST hold whatever they believe.
    Unresolved,
    /// Resolved: the foreground pgid runs something matching no manifest.
    /// This IS positive evidence that no known agent occupies the pane.
    Vacant {
        /// The process group we successfully looked at.
        ///
        /// Carried for tracing and for the tests that assert a real kernel was
        /// consulted; nothing compares it, which is why a vacant pane never
        /// pays for the start-time query.
        pgid: i32,
    },
    /// Resolved: an agent of `kind` owns the foreground pgid.
    Agent {
        /// Open-vocabulary kind slug, e.g. `"claude"`.
        kind: String,
        /// Who is running it. Retained so a *restart* of the same kind in the
        /// same pane is distinguishable from the original — it is otherwise
        /// invisible by construction — and start-time-paired so a recycled
        /// pgid cannot impersonate its predecessor.
        occupant: Occupant,
    },
}

/// Privacy-bounded description of the pane's foreground process.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PaneOccupant {
    /// Login-dash-stripped basename of `argv[0]`.
    pub(crate) foreground: String,
    /// The foreground group is the pane's original child and that child is a
    /// known interactive shell.
    pub(crate) is_pane_shell: bool,
}

impl Occupancy {
    /// The process group this occupancy is ABOUT, if the question was
    /// answered. [`Self::Unresolved`] carries none — unresolved is not
    /// evidence of anything, including a pgid.
    ///
    /// This is what [`AgentDetector`](super::AgentDetector)'s cheap
    /// pgid-only probe (phux-w7z2.50) compares against the last full
    /// resolution to decide whether the expensive argv read is even worth
    /// paying for on a given tick.
    pub(crate) const fn pgid(&self) -> Option<i32> {
        match self {
            Self::Unresolved => None,
            Self::Vacant { pgid } => Some(*pgid),
            Self::Agent { occupant, .. } => Some(occupant.pgid),
        }
    }
}

/// The cheap half of [`foreground_occupancy`]: the foreground process group
/// only, with no argv read.
///
/// `tcgetpgrp` is one ioctl; `process_argv` is a `/proc` read on Linux and two
/// `sysctl`s on macOS, and argv only changes when the pgid does. Polling this
/// on every ordinary detector tick and reserving [`foreground_occupancy`] for
/// when the pgid actually moved (or the periodic recheck is due) is the
/// phux-w7z2.50 fix: acquisition of an agent typed at an existing pane's
/// shell drops from up to one full [`super::IDENTIFY_RECHECK`] to the
/// ordinary tick cadence, for the cost of one extra ioctl on the ticks where
/// nothing changed.
///
/// Deliberately does NOT read the start time: that is only ever compared
/// once an [`Occupancy::Agent`] is already in hand (see the module docs), and
/// this function exists precisely to avoid paying for anything past the
/// pgid when nothing downstream needs it yet.
pub(crate) fn foreground_pgid(master_fd: Option<i32>) -> Option<i32> {
    proc_query::foreground_pgid(master_fd?)
}

/// Who occupies the foreground of this PTY.
///
/// 1. Ask the kernel which process group owns the tty.
/// 2. Read that process's argv.
/// 3. Resolve a kind from argv, in two tiers:
///
/// **Tier 1 — basename.** The basename of `argv[0]`, with any script suffix
/// stripped, matched against the manifests' `binaries`. When `argv[0]` is a
/// [runtime wrapper](RUNTIME_WRAPPERS), every later argument's basename is
/// tried too. This catches the native `claude` binary.
///
/// **Tier 2 — program-path components.** For arguments that are unambiguously
/// a *program path* — `argv[0]` when it contains a `/`, and any wrapper
/// argument whose basename carries a script suffix — each path component is
/// matched too. This catches `node .../claude-code/cli.js`, whose basename
/// (`cli`) is far too generic to list as a binary name, and the
/// version-pinned native install (`.../share/claude/versions/2.1.207`),
/// whose basename is a version number.
///
/// Tier 2 is deliberately NOT applied to arbitrary arguments. `sh -c 'cd
/// ~/.claude/foo && make'` must not identify as an agent just because a
/// user's *data* path contains the word — so a plain string argument is
/// never split into path components. Only the program's own path is.
///
/// First hit wins. A pane whose foreground process matches nothing is
/// [`Occupancy::Vacant`] — an answer, not a shrug; any step that fails to
/// produce an answer is [`Occupancy::Unresolved`].
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
            // Queried HERE and not before the match: a pane running a shell is
            // the common case, its pgid is compared to nothing, and the whole
            // point of the cadence budget is that an unoccupied pane costs one
            // timer wakeup.
            occupant: Occupant::new(pgid, crate::proc_query::process_start_time(pgid)),
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

/// The rule-matching core of [`foreground_occupancy`], split out so it is a pure
/// function of `(argv, rules)` and can be exhaustively table-tested without
/// a live process.
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

/// Match one *program-shaped* argument against the rule set: tier 1 on its
/// basename, then tier 2 on its path components when it is unambiguously a
/// program path.
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

/// Whether `arg` is unambiguously the path of a program (as opposed to a
/// data path, a flag, or a shell command string). Requires a `/` — a bare
/// name is handled by tier 1 — and no whitespace, which a `sh -c` command
/// string would carry.
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
    // A login shell arrives as `-zsh`; strip the leading dash before
    // comparing, or an interactive shell would never be recognized as a
    // wrapper and we would stop scanning at argv[0].
    let name = basename(arg).trim_start_matches('-');
    RUNTIME_WRAPPERS.contains(&name)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::{Occupancy, Occupant, foreground_occupancy, kind_from_argv, pane_occupant};
    use crate::agent_detect::rules::{ManifestSpec, RuleSet};

    fn rules() -> RuleSet {
        let spec: ManifestSpec = toml::from_str(
            r#"
kind = "claude"
binaries = ["claude", "claude-code"]
"#,
        )
        .expect("manifest parses");
        let mut set = RuleSet::default();
        set.install(spec).expect("compiles");
        set
    }

    #[test]
    fn pane_shell_requires_the_child_pgid_and_a_known_shell() {
        let login = pane_occupant(42, Some(42), &["-zsh".to_owned()]);
        assert_eq!(login.foreground, "zsh");
        assert!(login.is_pane_shell);

        assert!(!pane_occupant(43, Some(42), &["zsh".to_owned()]).is_pane_shell);
        assert!(!pane_occupant(42, Some(42), &["vim".to_owned()]).is_pane_shell);
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    fn kind(parts: &[&str]) -> Option<String> {
        kind_from_argv(&argv(parts), &rules())
    }

    #[test]
    fn native_binary_matches_on_argv0_basename() {
        assert_eq!(kind(&["claude"]).as_deref(), Some("claude"));
        assert_eq!(kind(&["/usr/local/bin/claude"]).as_deref(), Some("claude"));
    }

    #[test]
    fn version_pinned_native_install_matches_on_a_path_component() {
        // The real shape of a `claude` install: the bin entry is a symlink to
        // a version-numbered file, so the BASENAME is "2.1.207".
        assert_eq!(
            kind(&["/home/u/.local/share/claude/versions/2.1.207"]).as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn node_hosted_install_matches_through_the_wrapper_on_a_path_component() {
        // The npm shape. The basename is `cli`, which is far too generic to
        // ever list as a binary name; the package directory is the signal.
        assert_eq!(
            kind(&[
                "node",
                "/home/u/.npm/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            ])
            .as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn wrapper_flags_are_skipped() {
        assert_eq!(
            kind(&["node", "--enable-source-maps", "/opt/claude-code/cli.js"]).as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn login_shell_is_recognized_as_a_wrapper_and_yields_nothing() {
        assert_eq!(kind(&["-zsh"]), None);
        assert_eq!(kind(&["/bin/zsh"]), None);
    }

    /// The regression this design exists to prevent: a shell command string
    /// that merely *mentions* an agent-shaped path must NOT identify as that
    /// agent. A bogus agent row in the sidebar is a real bug, not a
    /// harmless one.
    #[test]
    fn shell_command_string_naming_a_data_path_does_not_identify() {
        assert_eq!(kind(&["sh", "-c", "cd /home/u/claude/notes && make"]), None);
        assert_eq!(kind(&["bash", "-c", "grep -r claude ."]), None);
    }

    /// A data path handed to a non-wrapper program is never even considered.
    #[test]
    fn a_non_wrapper_program_is_not_unwrapped() {
        assert_eq!(kind(&["vim", "/home/u/claude/notes.md"]), None);
        assert_eq!(kind(&["cat", "/opt/claude-code/cli.js"]), None);
    }

    #[test]
    fn unrelated_programs_do_not_identify() {
        assert_eq!(kind(&["htop"]), None);
        assert_eq!(kind(&["node", "/opt/other/server.js"]), None);
        assert_eq!(kind(&[]), None);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(kind(&["CLAUDE"]).as_deref(), Some("claude"));
    }

    // --- occupancy: the un-answerable question is its own value ------------

    /// A pane with no PTY answers nothing. Collapsing this into "no agent
    /// here" is what turned every unreadable pane into evidence that the
    /// agent died — and, once a retraction can withdraw a human's
    /// declaration, into a badge that vanishes because a `sysctl` blipped.
    #[test]
    fn a_pane_with_no_master_fd_is_unresolved_never_vacant() {
        assert_eq!(
            foreground_occupancy(None, &rules()),
            Occupancy::Unresolved,
            "no fd is not an observation",
        );
    }

    #[test]
    fn a_dead_or_bogus_fd_is_unresolved() {
        assert_eq!(
            foreground_occupancy(Some(-1), &rules()),
            Occupancy::Unresolved
        );
        assert_eq!(
            foreground_occupancy(Some(i32::MAX), &rules()),
            Occupancy::Unresolved,
        );
    }

    /// A regular file is a live fd that is simply not a tty, so the pgid
    /// query fails. Still unresolved: we learned nothing about occupancy.
    #[test]
    fn a_non_tty_fd_is_unresolved() {
        use std::os::fd::AsRawFd;
        let file = tempfile::tempfile().expect("temp file");
        assert_eq!(
            foreground_occupancy(Some(file.as_raw_fd()), &rules()),
            Occupancy::Unresolved,
        );
    }

    // --- against a real kernel ---------------------------------------------

    /// Spawn `program` in a real PTY and resolve its occupancy once the child
    /// is genuinely running it.
    ///
    /// Two synchronizations, and both are load-bearing. The child prints a
    /// banner, so the measurement happens after `execve` — a `fork`ed child
    /// that has taken the terminal but not yet replaced its image still
    /// carries the TEST BINARY's argv, and sampling there measures the
    /// harness. Then the foreground pgid must actually be the child's, because
    /// until `tcsetpgrp` runs `tcgetpgrp` answers for whoever held it before.
    #[cfg(unix)]
    fn occupancy_of(program: &std::path::Path, rules: &RuleSet) -> Occupancy {
        use std::io::Read as _;
        use std::sync::mpsc;
        use std::time::Duration;

        use portable_pty::{CommandBuilder, PtySize, native_pty_system};

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let mut child = pair
            .slave
            .spawn_command(CommandBuilder::new(program))
            .expect("spawn");
        let child_pid =
            i32::try_from(child.process_id().expect("a live child has a pid")).expect("pid fits");
        let fd = pair.master.as_raw_fd().expect("a real pty has a raw fd");

        // Wait for the banner: proof that the script's image is live.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 128];
            let mut seen = Vec::new();
            while let Ok(read) = reader.read(&mut buf) {
                if read == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..read]);
                if seen.windows(READY.len()).any(|w| w == READY.as_bytes()) {
                    let _ = tx.send(());
                    return;
                }
            }
        });
        let started = rx.recv_timeout(Duration::from_secs(10)).is_ok();

        let mut seen = Occupancy::Unresolved;
        for _ in 0..100 {
            seen = foreground_occupancy(Some(fd), rules);
            let pgid = match &seen {
                Occupancy::Unresolved => None,
                Occupancy::Vacant { pgid } => Some(*pgid),
                Occupancy::Agent { occupant, .. } => Some(occupant.pgid),
            };
            if pgid == Some(child_pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();

        assert!(started, "the child never printed its banner");
        let measured = match &seen {
            Occupancy::Unresolved => None,
            Occupancy::Vacant { pgid } => Some(*pgid),
            Occupancy::Agent { occupant, .. } => Some(occupant.pgid),
        };
        assert_eq!(
            measured,
            Some(child_pid),
            "the child never took the terminal; nothing was measured: {seen:?}",
        );
        seen
    }

    /// The banner [`occupancy_of`] waits for.
    #[cfg(unix)]
    const READY: &str = "phux-ready";

    /// Write an executable script named `name` into `dir` that announces
    /// itself and then idles.
    #[cfg(unix)]
    fn write_script(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho {READY}\nsleep 30\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// The pgid must be the KERNEL's, not a placeholder. It was previously
    /// resolved and discarded, which is what made a same-kind restart in one
    /// pane undetectable by construction — the detector had nothing to compare.
    #[cfg(unix)]
    #[test]
    fn a_live_agent_in_a_real_pty_carries_its_real_process_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent = write_script(dir.path(), "claude");
        match occupancy_of(&agent, &rules()) {
            Occupancy::Agent { kind, occupant } => {
                assert_eq!(kind, "claude", "identity comes from argv[0]");
                assert!(
                    occupant.pgid > 0,
                    "a real process group id, not a placeholder"
                );
            }
            other => panic!("a live agent in a real pty must resolve: {other:?}"),
        }
    }

    /// The .43 half, against a real kernel: an identified occupant carries the
    /// group leader's START TIME, not merely its pgid. Without it the identity
    /// is a recycled small integer, and a `codex` handed `claude`'s old pgid
    /// reads as the same occupant.
    ///
    /// Asserted only on the platforms that implement the query, because
    /// `started: None` is a legal answer everywhere else and this is the one
    /// test that would otherwise pass by accident on all of them.
    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn a_live_agent_carries_the_start_time_that_makes_its_pgid_unique() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent = write_script(dir.path(), "claude");
        match occupancy_of(&agent, &rules()) {
            Occupancy::Agent { occupant, .. } => {
                let started = occupant
                    .started
                    .expect("linux and macos both answer this question");
                assert!(started > 0, "a real start time, not a placeholder");
                // And a second reading of the SAME process agrees, or every
                // identity recheck would fabricate a restart.
                assert!(
                    occupant.same(Occupant::new(occupant.pgid, Some(started))),
                    "a stable reading must not read as a new occupant",
                );
            }
            other => panic!("a live agent in a real pty must resolve: {other:?}"),
        }
    }

    // --- occupant identity is the PAIR (phux-w7z2.43) ----------------------

    /// THE hole .43 exists to close. The OS recycles pgids; a new process of
    /// the same kind handed its predecessor's id is a different occupant with
    /// a different transcript, and comparing the integers alone cannot see it.
    #[test]
    fn a_recycled_pgid_with_a_different_start_time_is_a_different_occupant() {
        let first = Occupant::new(4242, Some(1_000));
        let recycled = Occupant::new(4242, Some(2_000));
        assert!(
            !first.same(recycled),
            "same id, different process: the start time is the whole point",
        );
    }

    #[test]
    fn the_same_pgid_and_start_time_is_the_same_occupant() {
        let occupant = Occupant::new(4242, Some(1_000));
        assert!(occupant.same(Occupant::new(4242, Some(1_000))));
    }

    #[test]
    fn a_different_pgid_is_a_different_occupant_whatever_the_start_time() {
        assert!(!Occupant::new(1, Some(9)).same(Occupant::new(2, Some(9))));
        assert!(!Occupant::new(1, None).same(Occupant::new(2, None)));
    }

    /// The module's rule applied one level down: a start time we could not
    /// read is not evidence of a restart. A platform that never answers, or a
    /// query that transiently fails, must degrade to the pre-.43 behaviour —
    /// compare pgids — and NOT to "everything is a new occupant", which would
    /// be a correction and a metadata write on every single identity recheck
    /// of every pane (ADR-0046 decision 7).
    #[test]
    fn an_unreadable_start_time_is_never_evidence_of_a_restart() {
        let known = Occupant::new(4242, Some(1_000));
        let unknown = Occupant::new(4242, None);
        assert!(known.same(unknown), "we did not learn anything: hold");
        assert!(unknown.same(known), "and it is symmetric");
        assert!(unknown.same(Occupant::new(4242, None)));
    }

    /// And the positive-vacancy half against a real kernel: a pane running
    /// something that is not an agent is an ANSWER — the evidence a withdrawal
    /// is allowed to act on — and not the same value as an unreadable pane.
    #[cfg(unix)]
    #[test]
    fn a_live_non_agent_in_a_real_pty_is_vacant_not_unresolved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let other = write_script(dir.path(), "definitely-not-an-agent");
        match occupancy_of(&other, &rules()) {
            Occupancy::Vacant { pgid } => assert!(pgid > 0),
            other => panic!("a live non-agent must be observed as vacant: {other:?}"),
        }
    }
}
