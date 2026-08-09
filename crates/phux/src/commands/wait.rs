use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_client::wait::{Condition, MatchRegex, MatchScope};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

/// Row count a bare `--tail` requests.
///
/// clap needs a `&'static str` in the attribute, so the flag's
/// `default_missing_value` is spelled as a literal there; this const is the
/// typed value and [`tests::bare_tail_matches_the_documented_constant`] pins
/// the two together, along with the `phux-core` constant both mirror.
#[cfg(test)]
const BARE_TAIL_ROWS: u32 = phux_core::screen::ROW_WINDOW_DEFAULT;

/// Everything `phux wait` needs, gathered so the entry point stays under
/// the argument-count lint (the `rec` pattern).
pub(crate) struct WaitArgs<'a> {
    /// Target selector, or `None` for the most-recently-focused session.
    pub(crate) session: Option<&'a str>,
    /// `--until TEXT`: literal substring.
    pub(crate) until: Option<String>,
    /// `--regex PATTERN`, already compiled by clap's value parser.
    pub(crate) regex: Option<MatchRegex>,
    /// `--idle MS`.
    pub(crate) idle: Option<u64>,
    /// `--tail N`: match within the last N logical lines.
    pub(crate) tail: Option<u32>,
    /// `--output-only`: skip the shell's echo of typed input.
    pub(crate) output_only: bool,
    /// `--timeout SECS`.
    pub(crate) timeout: Option<u64>,
    /// `--json`.
    pub(crate) json: bool,
    /// `--socket PATH`.
    pub(crate) socket: Option<PathBuf>,
}

/// The condition `args` asks for.
///
/// A text condition takes precedence over `--idle`; `--until` and `--regex`
/// are mutually exclusive at the clap layer, so the order between them here
/// is unreachable rather than a policy.
fn condition_for(args: &mut WaitArgs<'_>) -> Condition {
    use phux_client::wait::DEFAULT_IDLE_DWELL;

    if let Some(text) = args.until.take() {
        return Condition::Contains(text);
    }
    if let Some(pattern) = args.regex.take() {
        return Condition::Matches(pattern);
    }
    Condition::Idle(args.idle.map_or(DEFAULT_IDLE_DWELL, Duration::from_millis))
}

/// The stderr note printed when `--output-only` has nothing to filter on.
///
/// Not a refusal: refusing would fail a wait that is otherwise perfectly
/// good, and a hung wait is the exact failure this verb exists to remove.
/// Fail open, and say so.
const NO_MARKS_NOTE: &str = "phux: wait --output-only: this pane reports no OSC-133 shell marks, \
     so no line can be identified as your typed input; matching every line. \
     Enable shell integration, or match on text that appears only in output.";

/// `phux wait [TARGET]` — poll until a pane meets a condition (ADR-0022 §4).
///
/// `--until TEXT` waits for a line to contain `TEXT`, `--regex PATTERN` for
/// one to match `PATTERN`, `--idle MS` for the screen to settle; with none
/// of them, defaults to idle. Exits 0 when met, 124 on `--timeout`. The poll
/// floor of the event surface: it reads via the side-effect-free
/// `GET_SCREEN`, so it never disturbs the pane.
///
/// Matching is against the lines as **written** — soft-wrapped rows joined
/// (`phux_client::wait::match_lines`) — so a needle that straddles the
/// terminal's right edge is found instead of running the wait to timeout.
pub(crate) fn run_wait(mut args: WaitArgs<'_>) -> ExitCode {
    use phux_client::wait::{DEFAULT_POLL_INTERVAL, WaitOutcome};

    let selector = match parse_selector(args.session) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let json = args.json;
    let scope = MatchScope {
        tail: args.tail,
        output_only: args.output_only,
    };
    let condition = condition_for(&mut args);
    let timeout = args.timeout.map(Duration::from_secs);
    let socket_path = args.socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let terminal_id = match resolve_target(&socket_path, &selector, "wait", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        // One pre-flight read, only under `--output-only`, so the "nothing to
        // filter on" note lands NOW rather than after an unbounded wait — or
        // never, if the wait never returns. A failed probe is not reported
        // here; the poll loop hits the same failure a moment later and owns
        // the diagnostic.
        if scope.output_only
            && let Ok(screen) = phux_client::snapshot::get_screen_scrollback(
                &socket_path,
                terminal_id.clone(),
                scope.history_request(),
                true,
            )
            .await
            && !phux_client::wait::has_semantic_marks(&screen)
        {
            eprintln!("{NO_MARKS_NOTE}");
        }
        let result = match phux_client::wait::poll_until_scoped(
            &socket_path,
            terminal_id,
            &condition,
            timeout,
            DEFAULT_POLL_INTERVAL,
            &scope,
        )
        .await
        {
            Ok(result) => result,
            Err(err @ AttachError::Io(_)) => {
                return json_err::report_no_server(json, &err, &socket_path, "wait");
            }
            Err(err) => {
                eprintln!("phux: wait failed: {err}");
                return ExitCode::FAILURE;
            }
        };
        if json && let Ok(s) = serde_json::to_string_pretty(&result.screen) {
            outln!("{s}");
        }
        match result.outcome {
            WaitOutcome::Met => ExitCode::SUCCESS,
            WaitOutcome::TimedOut => {
                eprintln!("phux: wait timed out after {} polls", result.polls);
                ExitCode::from(crate::exit_codes::EXIT_WAIT_TIMEOUT)
            }
        }
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use clap::Parser as _;

    use super::*;
    use crate::Cli;

    fn args() -> WaitArgs<'static> {
        WaitArgs {
            session: None,
            until: None,
            regex: None,
            idle: None,
            tail: None,
            output_only: false,
            timeout: None,
            json: false,
            socket: None,
        }
    }

    /// Parse a `phux wait …` invocation into its variant fields.
    fn parse_wait(argv: &[&str]) -> crate::commands::Command {
        Cli::try_parse_from(argv)
            .expect("invocation should parse")
            .command
            .expect("a subcommand")
    }

    #[test]
    fn until_takes_precedence_over_idle() {
        let mut a = WaitArgs {
            until: Some("DONE".to_owned()),
            idle: Some(750),
            ..args()
        };
        assert!(matches!(
            condition_for(&mut a),
            Condition::Contains(text) if text == "DONE"
        ));
    }

    #[test]
    fn regex_takes_precedence_over_idle() {
        let mut a = WaitArgs {
            regex: Some("ok$".parse().expect("valid pattern")),
            idle: Some(750),
            ..args()
        };
        assert!(matches!(
            condition_for(&mut a),
            Condition::Matches(pattern) if pattern.as_str() == "ok$"
        ));
    }

    #[test]
    fn no_text_condition_settles_on_idle() {
        let mut a = WaitArgs {
            idle: Some(750),
            ..args()
        };
        assert!(matches!(
            condition_for(&mut a),
            Condition::Idle(dwell) if dwell == Duration::from_millis(750)
        ));
        let mut bare = args();
        assert!(matches!(
            condition_for(&mut bare),
            Condition::Idle(dwell) if dwell == phux_client::wait::DEFAULT_IDLE_DWELL
        ));
    }

    /// An invalid pattern is rejected by clap's value parser — a usage error
    /// with clap's own exit code 2, raised while parsing argv, so the
    /// process never connects to a server or performs a single poll.
    #[test]
    fn an_invalid_regex_is_a_usage_error_before_any_poll() {
        let err = Cli::try_parse_from(["phux", "wait", "--regex", "(unclosed", "build"])
            .expect_err("an invalid regex must not parse");
        assert_eq!(
            err.exit_code(),
            i32::from(crate::exit_codes::EXIT_USAGE),
            "an invalid --regex must exit 2 (usage), not run and fail later"
        );
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        let rendered = err.to_string();
        assert!(
            rendered.contains("--regex"),
            "the usage error should name the offending flag, got: {rendered}"
        );
    }

    #[test]
    fn a_valid_regex_parses_and_reaches_the_command() {
        let cmd =
            parse_wait(["phux", "wait", "--regex", r"^test result: ok\.", "build"].as_slice());
        let crate::commands::Command::Wait { regex, until, .. } = cmd else {
            panic!("expected the wait variant");
        };
        assert_eq!(
            regex.expect("a compiled pattern").as_str(),
            r"^test result: ok\."
        );
        assert!(until.is_none());
    }

    /// The two text conditions are alternatives, not a precedence puzzle the
    /// operator has to remember.
    #[test]
    fn until_and_regex_cannot_be_combined() {
        let err = Cli::try_parse_from(["phux", "wait", "--until", "ok", "--regex", "ok"])
            .expect_err("--until and --regex must conflict");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn bare_tail_matches_the_documented_constant() {
        let cmd = parse_wait(["phux", "wait", "--tail"].as_slice());
        let crate::commands::Command::Wait { tail, .. } = cmd else {
            panic!("expected the wait variant");
        };
        assert_eq!(tail, Some(BARE_TAIL_ROWS));
        assert_eq!(BARE_TAIL_ROWS, 80, "the flag help spells this literally");
    }

    /// An optional-value flag reads the next word as its value, so a bare
    /// `--tail` directly before TARGET swallows it. That is inherent to the
    /// same `num_args = 0..=1` shape `--scrollback` has always had; pinned
    /// here because the flag help promises the workaround (spell N out) and
    /// a loud usage error is the behavior we want, not a silent misparse.
    #[test]
    fn a_bare_tail_before_target_is_a_loud_usage_error() {
        let err = Cli::try_parse_from(["phux", "wait", "--tail", "build"])
            .expect_err("`--tail build` reads `build` as N");
        assert_eq!(err.exit_code(), i32::from(crate::exit_codes::EXIT_USAGE));
        let cmd = parse_wait(["phux", "wait", "--tail", "80", "build"].as_slice());
        let crate::commands::Command::Wait { tail, session, .. } = cmd else {
            panic!("expected the wait variant");
        };
        assert_eq!(tail, Some(80));
        assert_eq!(session.as_deref(), Some("build"));
    }

    #[test]
    fn tail_zero_means_all_retained_history() {
        let cmd = parse_wait(["phux", "wait", "--tail", "0", "build"].as_slice());
        let crate::commands::Command::Wait { tail, .. } = cmd else {
            panic!("expected the wait variant");
        };
        assert_eq!(tail, Some(phux_core::screen::ROW_WINDOW_ALL));
    }

    /// The scope the flags build is what drives the read: no `--tail` means
    /// no history request, and no `--output-only` means no cells — the poll
    /// stays as cheap as it has always been unless asked otherwise.
    #[test]
    fn flags_build_the_match_scope() {
        let bare = MatchScope::default();
        assert_eq!(bare.history_request(), None);
        assert!(!bare.wants_cells());

        let scoped = MatchScope {
            tail: Some(200),
            output_only: true,
        };
        assert_eq!(scoped.history_request(), Some(200));
        assert!(scoped.wants_cells());
    }

    #[test]
    fn the_no_marks_note_names_the_flag_and_the_remedy() {
        assert!(NO_MARKS_NOTE.contains("--output-only"));
        assert!(NO_MARKS_NOTE.contains("shell integration"));
    }
}
