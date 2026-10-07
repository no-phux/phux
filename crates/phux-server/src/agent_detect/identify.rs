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
use crate::proc_query;

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
pub(crate) struct PaneOccupant {
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
        // A version-numbered install: the basename is "2.1.207".
        assert_eq!(
            kind(&["/home/u/.local/share/claude/versions/2.1.207"]).as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn node_hosted_install_matches_through_the_wrapper_on_a_path_component() {
        // npm: the basename `cli` is too generic; the package dir signals.
        assert_eq!(
            kind(&[
                "node",
                "/home/u/.npm/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            ])
            .as_deref(),
            Some("claude")
        );
    }

    /// Gemini CLI's foreground argv as observed from 0.63.0 under npx: the
    /// pgid leader is `node <bin>/gemini`, its heap-sized relaunch shares the
    /// group, and the agent-tools plugin launches it behind its wrapper.
    #[test]
    fn shipped_rules_identify_the_observed_gemini_argv() {
        let rules = crate::agent_detect::rules::global();
        let bin = "/Users/u/.npm/_npx/d07ada7b4a99c96e/node_modules/.bin/gemini";
        for parts in [
            &["node", bin][..],
            &["/opt/node/bin/node", "--max-old-space-size=24576", bin][..],
            &[
                "sh",
                "/plugins/agent-tools/scripts/phux-agent-wrap.sh",
                "--name",
                "gemini",
                "--kind",
                "gemini",
                "--",
                "gemini",
            ][..],
        ] {
            assert_eq!(
                kind_from_argv(&argv(parts), &rules).as_deref(),
                Some("gemini"),
                "{parts:?}"
            );
        }
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

    /// A shell command merely mentioning an agent path is not that agent.
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

    /// No PTY means unresolved, not vacant.
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

    /// A non-tty fd is unresolved too.
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

    /// Spawn `program` in a real PTY and resolve its occupancy once it has
    /// exec'd (it prints a banner) and owns the foreground group.
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

    /// Write an executable script that announces itself and idles.
    #[cfg(unix)]
    fn write_script(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho {READY}\nsleep 30\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// The pgid comes from the kernel.
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

    /// An identified occupant carries a stable leader start time (on
    /// platforms that implement the query).
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
                // A second reading of the same process agrees.
                assert!(
                    occupant.same(Occupant::new(occupant.pgid, Some(started))),
                    "a stable reading must not read as a new occupant",
                );
            }
            other => panic!("a live agent in a real pty must resolve: {other:?}"),
        }
    }

    // --- occupant identity is the PAIR (phux-w7z2.43) ----------------------

    /// A recycled pgid with a new start time is a different occupant.
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

    /// Unknown start times degrade to pgid comparison, not "all new".
    #[test]
    fn an_unreadable_start_time_is_never_evidence_of_a_restart() {
        let known = Occupant::new(4242, Some(1_000));
        let unknown = Occupant::new(4242, None);
        assert!(known.same(unknown), "we did not learn anything: hold");
        assert!(unknown.same(known), "and it is symmetric");
        assert!(unknown.same(Occupant::new(4242, None)));
    }

    /// A non-agent foreground is a vacancy answer, not unresolved.
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
