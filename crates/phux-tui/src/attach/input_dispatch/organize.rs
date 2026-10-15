//! `organize`, `organize-session`, and `organize-undo`: the magic button
//! (ADR-0170). The same [`phux_client::organize`] path `phux organize` runs,
//! on the attached server's dedicated control dial, never on the
//! output-bearing attach connection. The result is a toast.

use phux_client::organize::{self, Options, Outcome};
use phux_config::keybind::ResolvedAction;

use crate::attach::Dial;
use crate::attach::connection::Connection;
use crate::render::overlay::ToastOverlay;

use super::ctx::DispatchCtx;
use super::effects::ActionEffects;

/// Plan lines a toast shows before summarizing the rest.
const TOAST_PLAN_LINES: usize = 8;

/// What the button was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Intent {
    /// Organize every pane, or only `session`'s.
    Run {
        /// Only this session's panes may move.
        session: Option<String>,
    },
    /// Reverse the last run.
    Undo,
}

/// Turn the action into an [`Intent`] for the async effect pass.
pub(super) fn queue(action: &ResolvedAction, ctx: &DispatchCtx<'_>, effects: &mut ActionEffects) {
    effects.organize = Some(match action.action.as_str() {
        "organize-undo" => Intent::Undo,
        "organize-session" => Intent::Run {
            session: Some(ctx.session_name.clone()),
        },
        _ => Intent::Run { session: None },
    });
}

/// A finished background organize run, for the driver to toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundToast {
    /// The toast title.
    pub title: String,
    /// Its lines.
    pub lines: Vec<String>,
}

/// Start the run off the input loop: a passthrough notice now, the result
/// as a toast through `ctx.background_tx` when it lands. Without a channel
/// (tests) the run is awaited here.
#[allow(clippy::future_not_send, reason = "TUI state is current-thread bound")]
pub(super) async fn apply(intent: Option<Intent>, ctx: &mut DispatchCtx<'_>) {
    let Some(intent) = intent else {
        return;
    };
    let Some(dial) = ctx.control_dial.cloned() else {
        ctx.overlays.push(Box::new(ToastOverlay::new(
            "Organize failed",
            vec!["No dedicated control dial; nothing changed.".to_owned()],
            ctx.theme,
        )));
        return;
    };
    let Some(tx) = ctx.background_tx.cloned() else {
        let done = perform(dial, intent).await;
        *ctx.host_refresh_request = true;
        ctx.overlays.push(Box::new(ToastOverlay::new(
            done.title, done.lines, ctx.theme,
        )));
        return;
    };
    let notice = match intent {
        Intent::Undo => "Undoing the last organize run…",
        Intent::Run { .. } => "Organizing… the result will appear here.",
    };
    ctx.overlays.push(Box::new(ToastOverlay::passthrough(
        "Organize",
        vec![notice.to_owned()],
        ctx.theme,
    )));
    tokio::spawn(async move {
        let _ = tx.send(perform(dial, intent).await);
    });
}

/// Run `intent` on its own control connection to `dial`.
async fn perform(dial: Dial, intent: Intent) -> BackgroundToast {
    let host = matches!(dial, Dial::Uds(_)).then(phux_config::instance::local_host_name);
    let result = async {
        let mut conn = Connection::connect_dial(&dial)
            .await
            .map_err(|err| err.to_string())?;
        match intent {
            Intent::Run { session } => run(&mut conn, host.as_deref(), session).await,
            Intent::Undo => undo(&mut conn).await,
        }
    }
    .await;
    let (title, lines) = result.unwrap_or_else(|error| ("Organize failed".to_owned(), vec![error]));
    BackgroundToast { title, lines }
}

async fn run(
    conn: &mut Connection,
    host: Option<&str>,
    session: Option<String>,
) -> Result<(String, Vec<String>), String> {
    let config = phux_config::loader::load().unwrap_or_default();
    let (judge, key_error) = organize::configured_judge(config.organize, false).await;
    let options = Options {
        session,
        judge,
        min_confidence: organize::DEFAULT_MIN_CONFIDENCE,
        // The input loop waits on the result.
        jev_timeout: organize::INTERACTIVE_JEV_TIMEOUT,
    };
    let mut outcome = organize::run(conn, host, &options, false)
        .await
        .map_err(|err| format!("Nothing changed: {err}"))?;
    outcome.judge_error = outcome.judge_error.or(key_error);
    Ok((outcome.summary(), toast_lines(&outcome)))
}

/// The plan as the toast shows it: what moved, what failed, how to undo.
fn toast_lines(outcome: &Outcome) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(why) = &outcome.judge_error {
        lines.push(format!(
            "Jev unavailable ({why}); used repository matching."
        ));
    }
    let plan = organize::plan_lines(&outcome.plan);
    if plan.is_empty() {
        lines.push("Every pane is already where it belongs.".to_owned());
    }
    lines.extend(plan.iter().take(TOAST_PLAN_LINES).cloned());
    if plan.len() > TOAST_PLAN_LINES {
        lines.push(format!("... and {} more", plan.len() - TOAST_PLAN_LINES));
    }
    if let Some(applied) = &outcome.applied {
        lines.extend(
            applied
                .failures
                .iter()
                .map(|failure| format!("failed: {}: {}", failure.step, failure.message)),
        );
        lines.extend(
            applied
                .skipped
                .iter()
                .map(|step| format!("skipped: {}: {}", step.step, step.message)),
        );
        if !applied.journal.is_empty() {
            lines.push("Undo: organize-undo, or `phux organize --undo`.".to_owned());
        }
    }
    if let Some(err) = &outcome.journal_error {
        lines.push(format!("Could not keep the undo record: {err}"));
    }
    lines
}

async fn undo(conn: &mut Connection) -> Result<(String, Vec<String>), String> {
    let journal = organize::load_journal()
        .map_err(|err| format!("Cannot read the undo record: {err}"))?
        .ok_or_else(|| "No organize run to undo.".to_owned())?;
    let undone = organize::undo(conn, &journal)
        .await
        .map_err(|err| format!("Nothing changed: {err}"))?;
    let mut lines = vec![
        organize::count(undone.journal.moved.len(), "pane", "moved back"),
        organize::count(undone.journal.renamed.len(), "session", "renamed back"),
        organize::count(undone.journal.created.len(), "session", "deleted"),
    ];
    lines.extend(
        undone
            .failures
            .iter()
            .map(|failure| format!("failed: {}: {}", failure.step, failure.message)),
    );
    lines.extend(
        undone
            .skipped
            .iter()
            .map(|step| format!("skipped: {}: {}", step.step, step.message)),
    );
    if undone.failures.is_empty() {
        organize::clear_journal().map_err(|err| format!("Undone, but: {err}"))?;
    }
    Ok(("Organize undone".to_owned(), lines))
}

#[cfg(test)]
mod tests {
    use phux_client_core::organize::{JudgeKind, Plan};

    use super::*;

    fn outcome(plan: Plan) -> Outcome {
        Outcome {
            total_panes: 0,
            plan,
            applied: None,
            judge_error: Some("HTTP 401".into()),
            journal_error: None,
        }
    }

    #[test]
    fn an_empty_plan_says_so_and_names_the_fallback() {
        let lines = toast_lines(&outcome(Plan {
            judge: JudgeKind::Heuristic,
            creates: Vec::new(),
            moves: Vec::new(),
            renames: Vec::new(),
            kept: Vec::new(),
        }));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("HTTP 401"));
        assert!(lines[1].contains("already where it belongs"));
    }
}
