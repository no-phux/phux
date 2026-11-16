use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_core::screen::{CellInfo, CellStyle, CursorState, ScreenState, SemanticContent, SoftWrap};
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::Scope;
use tokio::net::UnixListener;

use super::{RunOutcome, run_in_with_deadline};
use crate::agent_meta::{RESOURCE_PANE_OCCUPANT_KEY, ShellAvailability};
use crate::deadline::Deadline;
use crate::testkit::{self, ScriptSpec};

const PANE: u32 = 1;
const NONCE: &str = "n1";
/// Generous: the scripted server answers every read immediately, so a
/// regression fails on an assertion rather than on this bound.
const BUDGET: Duration = Duration::from_secs(10);

fn pane() -> ResourceId {
    ResourceId::local(PANE)
}

/// A pane whose cursor sits on an OSC-133 `Prompt` row — the shape the
/// precondition accepts — carrying `rows` as its viewport.
fn at_prompt(rows: &[&str]) -> ScreenState {
    let cursor_row = u16::try_from(rows.len().saturating_sub(1)).unwrap_or(0);
    ScreenState {
        pane: PANE,
        cols: 80,
        rows: 24,
        cursor: Some(CursorState {
            x: 0,
            y: cursor_row,
            visible: true,
        }),
        lines: rows.iter().map(|row| (*row).to_owned()).collect(),
        cells: Some(vec![CellInfo {
            col: 0,
            row: cursor_row,
            semantic: Some(SemanticContent::Prompt),
            style: CellStyle::default(),
        }]),
        ..ScreenState::default()
    }
}

fn occupant(foreground: &str, is_pane_shell: bool) -> Vec<u8> {
    format!(r#"{{"foreground":"{foreground}","is_pane_shell":{is_pane_shell}}}"#).into_bytes()
}

async fn run_against(socket: &Path, force: bool) -> RunOutcome {
    run_in_with_deadline(
        socket,
        pane(),
        "cargo build",
        NONCE,
        Deadline::new(Some(BUDGET)),
        force,
    )
    .await
    .expect("a scripted server answers every read")
}

/// Serve a fresh `make_spec` on each of the run's connections for the
/// whole of `body`.
async fn with_server<F, Fut, T>(make_spec: impl Fn() -> ScriptSpec + Send + 'static, body: F) -> T
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = T>,
{
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("run.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let peer = tokio::spawn(testkit::serve_every(listener, make_spec));
    let out = body(socket).await;
    peer.abort();
    out
}

/// A pane running `vim` is refused, naming the foreground, rather than
/// typed a shell line as normal-mode commands.
#[tokio::test]
async fn a_pane_running_vim_is_refused_before_anything_is_typed() {
    let outcome = with_server(
        || {
            ScriptSpec::new()
                .stored_metadata(
                    Scope::Resource(pane()),
                    RESOURCE_PANE_OCCUPANT_KEY,
                    occupant("vim", false),
                )
                .screen(&at_prompt(&["~", "~"]))
        },
        |socket| async move { run_against(&socket, false).await },
    )
    .await;

    let RunOutcome::Refused {
        command,
        availability,
    } = outcome
    else {
        panic!("expected a refusal, got {outcome:?}");
    };
    assert_eq!(command, "cargo build");
    assert_eq!(
        availability,
        ShellAvailability::BusyProcess("vim".to_owned())
    );
}

/// No occupant record and no OSC-133 marks: fail closed.
#[tokio::test]
async fn an_unanswerable_pane_is_refused_rather_than_typed_into() {
    let outcome = with_server(
        || ScriptSpec::new().screen(&ScreenState::default()),
        |socket| async move { run_against(&socket, false).await },
    )
    .await;
    assert!(
        matches!(
            outcome,
            RunOutcome::Refused {
                availability: ShellAvailability::Unanswerable,
                ..
            }
        ),
        "{outcome:?}"
    );
}

/// `force` types the command line without consulting the precondition.
#[tokio::test]
async fn force_types_the_command_line_into_a_busy_pane() {
    let outcome = with_server(
        || {
            ScriptSpec::new()
                .stored_metadata(
                    Scope::Resource(pane()),
                    RESOURCE_PANE_OCCUPANT_KEY,
                    occupant("vim", false),
                )
                .screen(&at_prompt(&[
                    &format!("PHUXrun{NONCE}BEGIN"),
                    "compiling",
                    &format!("PHUXrun{NONCE}RC=0=END"),
                ]))
        },
        |socket| async move { run_against(&socket, true).await },
    )
    .await;

    let RunOutcome::Completed(result) = outcome else {
        panic!("expected a completed run, got {outcome:?}");
    };
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.output, "compiling");
    assert!(!result.truncated);
}

/// In a pane narrower than the sentinels both markers soft-wrap; the run
/// still matches them on the joined rows instead of timing out.
#[tokio::test]
async fn sentinels_soft_wrapped_in_a_narrow_pane_still_complete() {
    let begin = format!("PHUXrun{NONCE}BEGIN");
    let rc = format!("PHUXrun{NONCE}RC=7=END");
    let (begin_head, begin_tail) = begin.split_at(6);
    let (rc_head, rc_tail) = rc.split_at(6);
    let mut narrow = at_prompt(&[begin_head, begin_tail, "out", rc_head, rc_tail]);
    narrow.cols = 6;
    narrow.soft_wrap = Some(SoftWrap {
        lines: vec![0, 3],
        scrollback: Vec::new(),
    });

    let outcome = with_server(
        move || {
            ScriptSpec::new()
                .stored_metadata(
                    Scope::Resource(pane()),
                    RESOURCE_PANE_OCCUPANT_KEY,
                    occupant("zsh", true),
                )
                .screen(&narrow)
        },
        |socket| async move { run_against(&socket, false).await },
    )
    .await;

    let RunOutcome::Completed(result) = outcome else {
        panic!("expected a completed run, got {outcome:?}");
    };
    assert_eq!(result.exit_code, 7);
    assert_eq!(result.output, "out");
    assert!(!result.truncated);
}

/// With `BEGIN` scrolled out of the viewport, the scrollback-aware
/// capture still returns the whole span, untruncated.
#[tokio::test]
async fn output_is_captured_across_the_scroll_boundary() {
    let mut scrolled = at_prompt(&["row 98", "row 99", &format!("PHUXrun{NONCE}RC=0=END")]);
    scrolled.scrollback = std::iter::once(format!("PHUXrun{NONCE}BEGIN"))
        .chain((0..98).map(|n| format!("row {n}")))
        .collect();

    let outcome = with_server(
        move || {
            ScriptSpec::new()
                .stored_metadata(
                    Scope::Resource(pane()),
                    RESOURCE_PANE_OCCUPANT_KEY,
                    occupant("zsh", true),
                )
                .screen(&scrolled)
        },
        |socket| async move { run_against(&socket, false).await },
    )
    .await;

    let RunOutcome::Completed(result) = outcome else {
        panic!("expected a completed run, got {outcome:?}");
    };
    let lines: Vec<&str> = result.output.lines().collect();
    assert_eq!(lines.len(), 100, "the whole BEGIN..RC span, not the tail");
    assert_eq!(lines.first(), Some(&"row 0"));
    assert_eq!(lines.last(), Some(&"row 99"));
    assert!(
        !result.truncated,
        "the span was captured in full, so nothing was truncated"
    );
}
