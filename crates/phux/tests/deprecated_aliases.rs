//! The deprecation contract, pinned at the binary level.
//!
//! Every row of `crate::deprecations::DEPRECATED` (the binary's own hidden
//! deprecation table — currently empty; see that module) must, for as long
//! as it exists:
//!
//!   * still parse (full arg surface) and run the replacement's
//!     implementation;
//!   * print EXACTLY ONE stderr deprecation line naming the visible
//!     replacement — and only on the human path: under `--json`, stdout
//!     carries only the document and a failure is one stderr contract
//!     line, so the note is suppressed on both;
//!   * be absent from `phux --help` and the generated shell completions,
//!     while its replacement is present.
//!
//! ADR-0066's `phux remote`/`phux satellite`/top-level `phux enroll` aliases
//! were the rows this file was written to audit; they were removed in
//! v0.12.1 once their deprecation window closed (phux-dpjf). The table-driven
//! tests below stay wired up — vacuously true over zero rows — for the next
//! spelling that needs one release cycle of warning before it goes.
//!
//! None of these verbs contact a server, so the tests are cheap and run in
//! the default pool (the `output_hygiene.rs` pattern).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::process::Command;

use tempfile::TempDir;

/// The canonical deprecation table, compiled straight from the binary's
/// own source (its module and items are `pub(crate)`, not part of the
/// `phux` lib crate's public API, so this integration test cannot reach it
/// through the ordinary `phux::` path): the same rows drive this audit,
/// the generated `docs/reference/deprecations.md` page, and the clap-tree
/// consistency test in `lib.rs`, so none can drift.
#[allow(dead_code, reason = "the audit consumes a subset of each row")]
#[allow(
    clippy::redundant_pub_crate,
    reason = "the file's pub(crate) is correct in its home crate, the phux lib"
)]
#[path = "../src/deprecations.rs"]
mod table;

use table::{DEPRECATED, DeprecatedSurface};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// Run `phux <args...>` under a scratch `XDG_CONFIG_HOME` and return
/// `(exit_code, stdout, stderr)`, with `dhat:` build diagnostics stripped
/// (the `--all-features` profile prints heap stats on clean exit).
fn run_with_xdg(args: &[&str], xdg_config_home: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", xdg_config_home)
        .args(args)
        .output()
        .expect("run phux binary");
    let stderr = String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|line| !line.starts_with("dhat: "))
        .fold(String::new(), |mut acc, line| {
            acc.push_str(line);
            acc.push('\n');
            acc
        });
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr,
    )
}

/// Assert `stderr` carries exactly the one deprecation line `expected`,
/// and no other mention of "deprecated" anywhere else on the stream.
fn assert_exact_note(stderr: &str, expected: &str) {
    let count = stderr.lines().filter(|line| *line == expected).count();
    assert_eq!(
        count, 1,
        "expected exactly one deprecation line {expected:?}; stderr={stderr:?}"
    );
    assert_eq!(
        stderr.matches("deprecated").count(),
        1,
        "exactly one deprecation mention; stderr={stderr:?}"
    );
}

// ---------------------------------------------------------------------------
// The table-driven audit (phux-i0e8.13.4): every row of
// `deprecations::DEPRECATED`, straight from the binary's own source, gets
// the same three assertions — parses-and-warns, hidden from --help, hidden
// from completions. The reverse guarantee (every hidden alias in the clap
// tree HAS a row here) is the tree-walk test in `lib.rs`, which shares
// this exact table.
// ---------------------------------------------------------------------------

/// Row by row: the old spelling still parses and prints its `note` exactly
/// once on stderr. Verb rows never contact a server and must succeed
/// outright; flag rows are pointed at a dead socket, so the row proves the
/// warning fires before (and regardless of) the connection failing — and
/// the non-2 exit code proves clap accepted the argv.
#[test]
fn every_table_row_parses_and_warns_once() {
    for row in DEPRECATED {
        let tmp = TempDir::new().expect("tempdir");
        let xdg = tmp.path().join("xdg");
        if !row.setup_argv.is_empty() {
            let (code, _, stderr) = run_with_xdg(row.setup_argv, &xdg);
            assert_eq!(code, 0, "setup for {}: stderr={stderr}", row.old);
        }
        match row.surface {
            DeprecatedSurface::Verb => {
                let (code, _, stderr) = run_with_xdg(row.example_argv, &xdg);
                assert_eq!(code, 0, "`{}` example: stderr={stderr}", row.old);
                assert_exact_note(&stderr, row.note);
            }
            DeprecatedSurface::Flag => {
                let dead = tmp.path().join("dead.sock");
                let dead = dead.to_str().expect("utf-8 temp path");
                let mut argv = row.example_argv.to_vec();
                argv.extend(["--socket", dead]);
                let (code, _, stderr) = run_with_xdg(&argv, &xdg);
                assert_ne!(code, 2, "`{}` must parse: stderr={stderr}", row.old);
                assert_ne!(
                    code, 0,
                    "`{}` against a dead socket must fail after warning",
                    row.old
                );
                assert_exact_note(&stderr, row.note);
            }
        }
    }
}

/// Row by row: the old spelling is absent from help. Verb rows must not be
/// listed by `phux --help`; flag rows must not show their boolean in the
/// carrying verb's own `--help`.
#[test]
fn no_table_row_surfaces_in_help() {
    let tmp = TempDir::new().expect("tempdir");

    let (code, top_help, _) = run_with_xdg(&["--help"], tmp.path());
    assert_eq!(code, 0);

    for row in DEPRECATED {
        match row.surface {
            DeprecatedSurface::Verb => {
                let verb = row.old_verb_path()[0];
                assert!(
                    !top_help
                        .lines()
                        .any(|line| line.trim_start().starts_with(&format!("{verb} "))),
                    "`phux --help` still advertises the hidden alias `{verb}`:\n{top_help}"
                );
            }
            DeprecatedSurface::Flag => {
                let verb = row.old_verb_path()[0];
                let flag = row.old_flag().expect("flag rows end in a long flag");
                let (code, help, _) = run_with_xdg(&[verb, "--help"], tmp.path());
                assert_eq!(code, 0, "`phux {verb} --help` must answer");
                assert!(
                    !help.contains(flag),
                    "`phux {verb} --help` still shows the deprecated `{flag}`:\n{help}"
                );
            }
        }
    }
}

/// Row by row: the old spelling is absent from the generated bash and zsh
/// completion scripts. Verb rows grep the joined path markers
/// `clap_complete` 4.6.7 actually emits (`phux__subcmd__<verb>`, which
/// cannot collide with the legitimate `host` subtree — see the completion
/// unit test in `lib.rs` for the G3 history); flag rows grep the long
/// flag itself, which no visible verb shares.
#[test]
fn no_table_row_surfaces_in_completions() {
    let tmp = TempDir::new().expect("tempdir");

    for shell in ["bash", "zsh"] {
        let (code, script, stderr) = run_with_xdg(&["completion", shell], tmp.path());
        assert_eq!(code, 0, "phux completion {shell}: stderr={stderr}");
        for row in DEPRECATED {
            let marker = match row.surface {
                DeprecatedSurface::Verb => {
                    format!("phux__subcmd__{}", row.old_verb_path()[0])
                }
                DeprecatedSurface::Flag => row
                    .old_flag()
                    .expect("flag rows end in a long flag")
                    .to_owned(),
            };
            assert!(
                !script.contains(&marker),
                "{shell} completions still offer `{}` (marker {marker:?})",
                row.old
            );
        }
    }
}
