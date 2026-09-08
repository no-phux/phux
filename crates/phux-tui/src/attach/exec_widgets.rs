//! phux-r82.6: per-client interval runners behind `exec` status widgets.
//!
//! The widget side (`phux_config::widget::ExecWidget`) only ever renders a
//! cached strip; this module is the host half that keeps the cache fresh.
//! One tokio task per [`ExecFeed`] runs the configured argv as a bounded
//! child process — `kill_on_drop` via [`phux_plugin::run_command_spec`],
//! exactly like plugin actions — and folds captured stdout into the feed.
//!
//! The render loop is never blocked: a run that hangs is killed at its
//! timeout, a failed run keeps the last good output, and the painter picks
//! updated cells up on its normal repaint tick (its row cache diffs the
//! new strip in). Dropping the returned [`ExecFeedRunners`] guard aborts
//! every task; `kill_on_drop` then reaps any in-flight child.

use std::time::Duration;

use phux_config::widget::ExecFeed;
use phux_plugin::{CommandSpec, PluginActionOutcome, run_command_spec};

/// Hard ceiling on one run's wall clock. A run is otherwise bounded by
/// its own interval (so runs never pile up), but a widget on a long
/// interval (say `5m`) should not hold a wedged child for five minutes.
const MAX_RUN_TIMEOUT: Duration = Duration::from_secs(10);

/// Owner of the spawned runner tasks. Aborting on drop ties the runners'
/// lifetime to the attach loop that spawned them — a detach/switch tears
/// the tasks (and via `kill_on_drop`, their children) down.
#[derive(Debug, Default)]
pub(super) struct ExecFeedRunners {
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for ExecFeedRunners {
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}

/// Spawn one interval runner per feed. An empty feed list (no `exec`
/// widgets configured) spawns nothing.
pub(super) fn spawn_exec_feed_runners(feeds: Vec<ExecFeed>) -> ExecFeedRunners {
    ExecFeedRunners {
        handles: feeds
            .into_iter()
            .map(|feed| tokio::spawn(run_feed(feed)))
            .collect(),
    }
}

/// Drive one feed forever: run immediately (so the bar populates without
/// waiting a full interval), then on every tick. `Delay` tick behavior —
/// a slow run pushes the next one out rather than bursting to catch up.
async fn run_feed(feed: ExecFeed) {
    let mut tick = tokio::time::interval(feed.interval());
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        run_once(&feed).await;
    }
}

/// Execute the feed's command once, bounded, and fold stdout into the
/// cache. Failures (spawn error, timeout) keep the previous output and
/// log — a broken widget script must never take down the attach loop.
pub(super) async fn run_once(feed: &ExecFeed) {
    let spec = CommandSpec {
        argv: feed.argv().to_vec(),
        cwd: None,
        env: Vec::new(),
        timeout: Some(feed.interval().min(MAX_RUN_TIMEOUT)),
    };
    match run_command_spec(spec).await {
        Ok(output) if output.outcome == PluginActionOutcome::Completed => {
            // Non-zero exits still render their stdout — statusline
            // scripts conventionally print what they can and exit non-zero
            // on partial data. The trace is there for debugging.
            if output.exit_code != Some(0) {
                tracing::debug!(
                    argv = ?feed.argv(),
                    exit_code = ?output.exit_code,
                    "exec widget command exited non-zero",
                );
            }
            feed.apply_output(&output.stdout);
        }
        Ok(_) => {
            tracing::warn!(
                argv = ?feed.argv(),
                "exec widget command timed out; keeping last output",
            );
        }
        Err(err) => {
            tracing::warn!(
                argv = ?feed.argv(),
                error = %err,
                "exec widget command failed to run; keeping last output",
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use phux_config::WidgetSpec;
    use phux_config::widget::{StatusWidget, WidgetContext, WidgetRegistry};
    use std::time::UNIX_EPOCH;

    fn exec_widget(cmd: &str) -> Box<dyn StatusWidget> {
        let spec = WidgetSpec {
            kind: "exec".to_owned(),
            opts: std::iter::once(("command".to_owned(), toml::Value::String(cmd.to_owned())))
                .collect(),
        };
        WidgetRegistry::with_builtins()
            .build(&spec)
            .expect("exec widget builds")
    }

    fn rendered(widget: &dyn StatusWidget) -> String {
        widget
            .render(&WidgetContext::new(UNIX_EPOCH, "", "C-a", &[]))
            .cells
            .iter()
            .filter_map(|c| c.text.first())
            .collect()
    }

    /// End-to-end through the real child-process path: one bounded run of
    /// a `/bin/sh -c` command lands its stdout in the widget's cache
    /// without the render loop's involvement.
    #[tokio::test]
    async fn run_once_feeds_command_stdout_into_the_widget() {
        let widget = exec_widget("printf 'BAT 87%%'");
        let feed = widget.exec_feed().expect("exec feed");
        assert_eq!(rendered(widget.as_ref()), "", "empty before the run");
        run_once(&feed).await;
        assert_eq!(rendered(widget.as_ref()), "BAT 87%");
    }

    /// A failing run keeps the previous output rather than blanking the
    /// bar (and must not panic the runner).
    #[tokio::test]
    async fn failed_run_keeps_last_output() {
        let widget = exec_widget("printf ok");
        let feed = widget.exec_feed().expect("exec feed");
        run_once(&feed).await;
        assert_eq!(rendered(widget.as_ref()), "ok");
        // A spawn failure: argv[0] does not exist (direct argv, no shell).
        let missing = phux_config::widget::ExecWidget::new(
            vec!["/nonexistent/phux-widget".to_owned()],
            Duration::from_secs(1),
            true,
        );
        let missing_feed = missing.exec_feed().expect("exec feed");
        run_once(&missing_feed).await;
        // The healthy widget's cache is untouched; the broken one stays empty.
        assert_eq!(rendered(widget.as_ref()), "ok");
        assert_eq!(rendered(&missing), "");
    }

    /// Non-zero exits still render their stdout (statusline convention).
    #[tokio::test]
    async fn non_zero_exit_still_applies_stdout() {
        let widget = exec_widget("printf degraded; exit 3");
        let feed = widget.exec_feed().expect("exec feed");
        run_once(&feed).await;
        assert_eq!(rendered(widget.as_ref()), "degraded");
    }
}
