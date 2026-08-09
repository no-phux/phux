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

use super::rules::RuleSet;
use crate::proc_query;

/// Interpreters that merely *host* an agent: the interesting name is
/// further along argv, not at `argv[0]`.
const RUNTIME_WRAPPERS: [&str; 14] = [
    "node", "nodejs", "bun", "deno", "python", "python3", "sh", "bash", "zsh", "fish", "env",
    "npx", "uv", "uvx",
];

/// Suffixes stripped from a script name before matching (`cli.js` -> `cli`).
const SCRIPT_SUFFIXES: [&str; 5] = [".js", ".mjs", ".cjs", ".py", ".ts"];

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
        pgid: i32,
    },
    /// Resolved: an agent of `kind` owns the foreground pgid.
    Agent {
        /// Open-vocabulary kind slug, e.g. `"claude"`.
        kind: String,
        /// The process group running it. Retained so a *restart* of the same
        /// kind in the same pane is distinguishable from the original — it
        /// is otherwise invisible by construction.
        pgid: i32,
    },
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
pub(crate) fn foreground_occupancy(master_fd: Option<i32>, rules: &RuleSet) -> Occupancy {
    let Some(fd) = master_fd else {
        return Occupancy::Unresolved;
    };
    let Some(pgid) = proc_query::foreground_pgid(fd) else {
        return Occupancy::Unresolved;
    };
    let Some(argv) = proc_query::process_argv(pgid) else {
        return Occupancy::Unresolved;
    };
    kind_from_argv(&argv, rules).map_or(Occupancy::Vacant { pgid }, |kind| Occupancy::Agent {
        kind,
        pgid,
    })
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
    use super::{Occupancy, foreground_occupancy, kind_from_argv};
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
                Occupancy::Vacant { pgid } | Occupancy::Agent { pgid, .. } => Some(*pgid),
            };
            if pgid == Some(child_pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();

        assert!(started, "the child never printed its banner");
        assert!(
            matches!(&seen, Occupancy::Vacant { pgid } | Occupancy::Agent { pgid, .. } if *pgid == child_pid),
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
            Occupancy::Agent { kind, pgid } => {
                assert_eq!(kind, "claude", "identity comes from argv[0]");
                assert!(pgid > 0, "a real process group id, not a placeholder");
            }
            other => panic!("a live agent in a real pty must resolve: {other:?}"),
        }
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
