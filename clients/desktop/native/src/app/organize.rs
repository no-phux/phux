//! Organize Everything: the desktop's one-step organize (ADR-0170).
//!
//! The judge runs off the UI thread; the runtime's
//! [`Sequencer`](phux_client_runtime::organize::Sequencer) then carries the
//! plan out over this window's organization lane, fed every operation
//! outcome, and keeps the shared undo record `phux organize --undo` and the
//! TUI read too.

use gpui::Context;
use phux_client_core::org_ops::OrgOutcome;
use phux_client_core::organize::{
    DEFAULT_MIN_CONFIDENCE, Facts, JudgeKind, Plan, journal_summary, plan_lines,
};
use phux_client_runtime::control::OrganizationReplica;
use phux_client_runtime::organize::{
    INTERACTIVE_JEV_TIMEOUT, Judge, Options, PaneSeen, PlanError, ProjectCache, Report, Sequencer,
    pane_facts, session_facts,
};
use phux_protocol::ResourceId;
use phux_protocol::ids::{OperationId, ResourceKind};

use super::desk::Desk;
use super::notifications::Kind;

/// Plan lines a notification shows before summarizing the rest.
const NOTICE_LINES: usize = 8;

/// An organize run (or its undo) in flight.
pub(crate) struct Run {
    sequencer: Sequencer,
    judge: Option<JudgeKind>,
    lines: Vec<String>,
}

impl Desk {
    /// Organize every pane: plan in the background, then apply.
    pub(crate) fn organize_everything(&mut self, cx: &mut Context<Self>) {
        if self.organization.busy() {
            self.post(Kind::Refusal, "Organize is already running", None);
            return;
        }
        let Some(client) = self.client() else { return };
        let host = self
            .remote
            .is_none()
            .then(phux_client_runtime::organize::local_host_name);
        let facts = self.organize_facts(&client.organization(), host.as_deref());
        self.organization.planning = true;
        self.post(Kind::Info, "Organizing…", None);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let planned = smol::unblock(move || plan(&facts)).await;
            let _ = this.update(cx, |desk, cx| {
                desk.organization.planning = false;
                desk.apply_organize(planned, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Undo the last organize run, from any surface on this profile.
    pub(crate) fn undo_organize_everything(&mut self, cx: &mut Context<Self>) {
        if self.organization.busy() {
            self.post(Kind::Refusal, "Organize is already running", None);
            return;
        }
        let journal = match phux_client_runtime::organize::load_journal() {
            Ok(Some(journal)) => journal,
            Ok(None) => {
                self.post(Kind::Refusal, "No organize run to undo", None);
                return;
            }
            Err(err) => {
                self.post(
                    Kind::Error,
                    "Couldn't read the organize undo record",
                    Some(err.to_string()),
                );
                return;
            }
        };
        self.start_run(Sequencer::undo(journal), None, Vec::new());
        cx.notify();
    }

    /// Route an organization outcome to the run in flight. Returns whether
    /// it was the run's.
    pub(crate) fn note_organize(&mut self, operation: OperationId, outcome: &OrgOutcome) -> bool {
        let Some(client) = self.client() else {
            return false;
        };
        let Some(run) = self.organization.run.as_mut() else {
            return false;
        };
        if !run.sequencer.owns(&operation) {
            return false;
        }
        let replica = client.organization();
        if let Some(report) =
            run.sequencer
                .on_outcome(operation, outcome, &replica, client.as_ref())
        {
            self.finish_run(report);
        }
        true
    }

    fn apply_organize(
        &mut self,
        planned: Result<(Plan, Option<String>), PlanError>,
        _cx: &mut Context<Self>,
    ) {
        let (plan, judge_error) = match planned {
            Ok(planned) => planned,
            Err(err) => {
                self.post(Kind::Error, "Couldn't organize", Some(err.to_string()));
                return;
            }
        };
        if let Some(why) = judge_error {
            self.post(
                Kind::Info,
                "Jev unavailable; organized by repository",
                Some(why),
            );
        }
        if plan.is_empty() {
            self.post(Kind::Info, "Every pane is where it belongs", None);
            return;
        }
        let Some(client) = self.client() else { return };
        let authority = client.organization().authority();
        let judge = plan.judge;
        let lines = plan_lines(&plan);
        self.start_run(Sequencer::apply(plan, authority), Some(judge), lines);
    }

    fn start_run(&mut self, sequencer: Sequencer, judge: Option<JudgeKind>, lines: Vec<String>) {
        let Some(client) = self.client() else { return };
        let mut run = Run {
            sequencer,
            judge,
            lines,
        };
        let replica = client.organization();
        let finished = run.sequencer.start(&replica, client.as_ref());
        self.organization.run = Some(Box::new(run));
        if let Some(report) = finished {
            self.finish_run(report);
        }
    }

    fn finish_run(&mut self, report: Report) {
        let Some(run) = self.organization.run.take() else {
            return;
        };
        let Some(judge) = run.judge else {
            self.finish_undo(&report);
            return;
        };
        if !report.journal.is_empty()
            && let Err(err) = phux_client_runtime::organize::save_journal(&report.journal)
        {
            self.post(
                Kind::Error,
                "Couldn't keep the organize undo record",
                Some(err.to_string()),
            );
        }
        let title = journal_summary(judge, &report.journal, report.failures.len());
        let mut body: Vec<String> = run.lines.iter().take(NOTICE_LINES).cloned().collect();
        if run.lines.len() > NOTICE_LINES {
            body.push(format!("… and {} more", run.lines.len() - NOTICE_LINES));
        }
        body.extend(step_lines(&report));
        let kind = if report.failures.is_empty() {
            Kind::Info
        } else {
            Kind::Error
        };
        self.post(kind, &title, Some(body.join("\n")));
    }

    fn finish_undo(&mut self, report: &Report) {
        if report.failures.is_empty()
            && let Err(err) = phux_client_runtime::organize::clear_journal()
        {
            self.post(
                Kind::Error,
                "Undone, but the record stayed",
                Some(err.to_string()),
            );
        }
        let journal = &report.journal;
        let mut body = vec![format!(
            "{} moved back, {} renamed back, {} deleted",
            journal.moved.len(),
            journal.renamed.len(),
            journal.created.len()
        )];
        body.extend(step_lines(report));
        let kind = if report.failures.is_empty() {
            Kind::Info
        } else {
            Kind::Error
        };
        self.post(kind, "Organize undone", Some(body.join("\n")));
    }

    /// Everything the planner reads, from this window's replica and panes.
    fn organize_facts(&self, replica: &OrganizationReplica, host: Option<&str>) -> Facts {
        let mut projects = ProjectCache::new(host);
        let sessions = replica
            .sessions()
            .iter()
            .map(|record| session_facts(record, replica.intake()))
            .collect();
        let panes = replica
            .resources()
            .iter()
            .filter(|record| record.kind != ResourceKind::AgentSession)
            .map(|record| {
                let seen = self.pane_seen(record.live.as_ref(), record.name.clone(), record.key);
                pane_facts(record.key, record.session, seen, &mut projects)
            })
            .collect();
        Facts { sessions, panes }
    }

    fn pane_seen(
        &self,
        live: Option<&ResourceId>,
        name: Option<String>,
        key: phux_protocol::ids::ResourceKey,
    ) -> PaneSeen {
        let connection = self.link.as_ref().map(|link| &link.connection);
        let pane = live.and_then(|id| connection?.pane(id));
        let agent = live.and_then(|id| connection?.agents.get(id));
        PaneSeen {
            label: live.and_then(ResourceId::local_id).map_or_else(
                || format!("^{}", &key.to_string()[..6]),
                |id| format!("@{id}"),
            ),
            title: name.or_else(|| pane.and_then(|pane| pane.title.clone())),
            cwd: pane.and_then(|pane| pane.cwd.clone()),
            agent: agent.map(|agent| format!("{} {:?}", agent.name, agent.state).to_lowercase()),
            status: pane.and_then(|pane| pane.exec).map_or_else(
                || "running".to_owned(),
                |exec| format!("{exec:?}").to_lowercase(),
            ),
        }
    }
}

/// Plan on a blocking thread: the judge the config allows, falling back to
/// repository matching.
fn plan(facts: &Facts) -> Result<(Plan, Option<String>), PlanError> {
    let (judge, key_error) = Judge::from_config_file(false);
    let options = Options {
        session: None,
        judge,
        min_confidence: DEFAULT_MIN_CONFIDENCE,
        jev_timeout: INTERACTIVE_JEV_TIMEOUT,
    };
    phux_client_runtime::organize::plan_or_fallback(facts, &options)
        .map(|(plan, why)| (plan, why.or(key_error)))
}

fn step_lines(report: &Report) -> Vec<String> {
    let failed = report
        .failures
        .iter()
        .map(|step| format!("failed: {}: {}", step.step, step.message));
    let skipped = report
        .skipped
        .iter()
        .map(|step| format!("skipped: {}: {}", step.step, step.message));
    failed.chain(skipped).collect()
}
