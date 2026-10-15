//! Carry out an organize plan, or undo one, over the runtime's asynchronous
//! organization lane: submissions now, outcomes later as events.
//!
//! [`Sequencer`] is sans-IO. A client hands it a [`Submitter`] (the runtime
//! [`Client`](crate::runtime::Client) is one) and the replica, and feeds it
//! every `OrganizationOperation` outcome; it answers with what finished.
//! Ordering is the one `phux_client::organize::apply` follows: creates,
//! then moves (re-checked against the replica) and renames; undo renames
//! and moves back, then deletes the sessions the run created.

use std::collections::{BTreeMap, HashMap, HashSet};

use phux_client_core::org_ops::{OrgOutcome, SubmitError, refusal_sentence};
use phux_client_core::organize::{CreatedSession, Journal, MovedPane, Plan, PlannedRename};
use phux_protocol::ids::{AuthorityId, OperationId, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{Command, MAX_ORG_BATCH, Refusal, SessionLifetime};

use crate::control::OrganizationReplica;

/// The organization submissions a [`Sequencer`] makes.
pub trait Submitter {
    /// Submit a typed command (here, `SESSION_CREATE`).
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    fn submit(&self, command: Command) -> Result<OperationId, SubmitError>;
    /// Move `resources` into `to`; one operation per server batch.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    fn move_resources(
        &self,
        resources: &[ResourceKey],
        to: SessionKey,
    ) -> Result<Vec<OperationId>, SubmitError>;
    /// Rename `session`.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    fn rename_session(&self, session: SessionKey, name: &str) -> Result<OperationId, SubmitError>;
    /// Delete `session`, refused while it has members.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    fn delete_session(&self, session: SessionKey) -> Result<OperationId, SubmitError>;
}

impl Submitter for crate::runtime::Client {
    fn submit(&self, command: Command) -> Result<OperationId, SubmitError> {
        self.submit_organization(command)
    }

    fn move_resources(
        &self,
        resources: &[ResourceKey],
        to: SessionKey,
    ) -> Result<Vec<OperationId>, SubmitError> {
        Self::move_resources(self, resources, to)
    }

    fn rename_session(&self, session: SessionKey, name: &str) -> Result<OperationId, SubmitError> {
        Self::rename_session(self, session, name)
    }

    fn delete_session(&self, session: SessionKey) -> Result<OperationId, SubmitError> {
        Self::delete_session(self, session)
    }
}

/// One step that failed or was left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// What the step was (`create web`, `move 3 terminals to web`).
    pub step: String,
    /// Why it did not take effect.
    pub message: String,
}

/// What a finished run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// What took effect.
    pub journal: Journal,
    /// Steps the server refused or that could not be sent.
    pub failures: Vec<Step>,
    /// Steps left alone because the records changed.
    pub skipped: Vec<Step>,
}

/// What an operation in flight will do when it lands.
#[derive(Debug, Clone)]
enum Pending {
    Create(String),
    Move {
        panes: Vec<MovedPane>,
        to_name: String,
    },
    Rename(PlannedRename),
    Delete(CreatedSession),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Apply: creating sessions.
    Creating,
    /// Apply: moves and renames in flight. Undo: renames and moves back.
    Editing,
    /// Undo: deleting the sessions the run created.
    Deleting,
    Done,
}

#[derive(Debug, Clone)]
enum Work {
    Apply(Box<Plan>),
    Undo(Journal),
}

/// One organize apply or undo in flight.
#[derive(Debug, Clone)]
pub struct Sequencer {
    work: Work,
    phase: Phase,
    /// Operations in flight; a multi-batch move shares one entry under each
    /// of its ids, credited when the last lands.
    pending: HashMap<OperationId, usize>,
    entries: Vec<(Pending, HashSet<OperationId>, bool)>,
    new_keys: HashMap<String, SessionKey>,
    report: Report,
}

impl Sequencer {
    /// Apply `plan` to the server `authority` names.
    #[must_use]
    pub fn apply(plan: Plan, authority: Option<AuthorityId>) -> Self {
        let mut journal = Journal::new();
        journal.authority = authority;
        Self::new(Work::Apply(Box::new(plan)), Phase::Creating, journal)
    }

    /// Undo `journal`.
    #[must_use]
    pub fn undo(journal: Journal) -> Self {
        Self::new(Work::Undo(journal), Phase::Editing, Journal::new())
    }

    fn new(work: Work, phase: Phase, journal: Journal) -> Self {
        Self {
            work,
            phase,
            pending: HashMap::new(),
            entries: Vec::new(),
            new_keys: HashMap::new(),
            report: Report {
                journal,
                ..Report::default()
            },
        }
    }

    /// Whether `operation` is one of this run's.
    #[must_use]
    pub fn owns(&self, operation: &OperationId) -> bool {
        self.pending.contains_key(operation)
    }

    /// Submit the first phase. Returns the report when nothing was left to
    /// wait for.
    pub fn start(
        &mut self,
        replica: &OrganizationReplica,
        submitter: &impl Submitter,
    ) -> Option<Report> {
        if let Work::Undo(journal) = &self.work
            && journal
                .authority
                .is_some_and(|authority| replica.authority() != Some(authority))
        {
            self.report.failures.push(Step {
                step: "undo".to_owned(),
                message: "the last organize run changed another server".to_owned(),
            });
            self.phase = Phase::Done;
            return Some(self.report.clone());
        }
        self.enter(replica, submitter)
    }

    /// Record `outcome` for `operation`; submits the next phase when this
    /// one is done. Returns the report once the run is finished.
    pub fn on_outcome(
        &mut self,
        operation: OperationId,
        outcome: &OrgOutcome,
        replica: &OrganizationReplica,
        submitter: &impl Submitter,
    ) -> Option<Report> {
        let index = self.pending.remove(&operation)?;
        let (pending, ops, ok) = &mut self.entries[index];
        ops.remove(&operation);
        *ok &= matches!(outcome, OrgOutcome::Committed(_));
        if let (Pending::Create(name), OrgOutcome::Committed(receipt)) = (&*pending, outcome)
            && let Some(key) = receipt.session
        {
            self.new_keys.insert(name.clone(), key);
        }
        if ops.is_empty() {
            let (pending, _, ok) = self.entries[index].clone();
            self.settle(pending, ok, outcome);
        }
        if !self.pending.is_empty() {
            return None;
        }
        self.phase = match (&self.work, self.phase) {
            (Work::Apply(_), Phase::Creating) => Phase::Editing,
            (Work::Undo(_), Phase::Editing) => Phase::Deleting,
            _ => Phase::Done,
        };
        self.enter(replica, submitter)
    }

    /// Submit the current phase, skipping empty ones.
    fn enter(
        &mut self,
        replica: &OrganizationReplica,
        submitter: &impl Submitter,
    ) -> Option<Report> {
        loop {
            match (self.work.clone(), self.phase) {
                (_, Phase::Done) => return Some(self.report.clone()),
                (Work::Apply(plan), Phase::Creating) => self.submit_creates(&plan, submitter),
                (Work::Apply(plan), Phase::Editing) => {
                    self.submit_moves(&plan, replica, submitter);
                    self.submit_renames(&plan.renames, submitter);
                }
                (Work::Undo(journal), Phase::Editing) => {
                    let back = reversed_renames(&journal, replica, &mut self.report);
                    self.submit_renames(&back, submitter);
                    self.submit_moves_back(&journal, replica, submitter);
                }
                (Work::Undo(journal), Phase::Deleting) => self.submit_deletes(&journal, submitter),
                (Work::Apply(_), Phase::Deleting) | (Work::Undo(_), Phase::Creating) => {}
            }
            if !self.pending.is_empty() {
                return None;
            }
            self.phase = match (&self.work, self.phase) {
                (Work::Apply(_), Phase::Creating) => Phase::Editing,
                (Work::Undo(_), Phase::Editing) => Phase::Deleting,
                _ => Phase::Done,
            };
        }
    }

    fn track(&mut self, pending: Pending, operations: Vec<OperationId>) {
        let index = self.entries.len();
        for operation in &operations {
            self.pending.insert(*operation, index);
        }
        self.entries
            .push((pending, operations.into_iter().collect(), true));
    }

    fn submit_creates(&mut self, plan: &Plan, submitter: &impl Submitter) {
        for new in &plan.creates {
            let command = Command::SessionCreate {
                operation: crate::control::mint_operation(),
                name: new.name.clone(),
                lifetime: SessionLifetime::Persistent,
                attrs: super::creation_attrs(new),
                launch: None,
            };
            match submitter.submit(command) {
                Ok(operation) => self.track(Pending::Create(new.name.clone()), vec![operation]),
                Err(err) => self.fail(format!("create {}", new.name), &err.to_string()),
            }
        }
    }

    fn submit_moves(
        &mut self,
        plan: &Plan,
        replica: &OrganizationReplica,
        submitter: &impl Submitter,
    ) {
        let mut batches: BTreeMap<SessionKey, (String, Vec<MovedPane>)> = BTreeMap::new();
        for step in &plan.moves {
            let label = format!("move {} to {}", step.label, step.to_name);
            if replica.resource(&step.pane).map(|r| r.session) != Some(step.from) {
                self.skip(label, "moved or gone since the plan");
                continue;
            }
            let Some(to) = step
                .to
                .or_else(|| self.new_keys.get(&step.to_name).copied())
            else {
                self.fail(
                    label,
                    &format!("session {:?} was not created", step.to_name),
                );
                continue;
            };
            let entry = batches
                .entry(to)
                .or_insert_with(|| (step.to_name.clone(), Vec::new()));
            entry.1.push(MovedPane {
                pane: step.pane,
                from: step.from,
                to,
            });
        }
        for (to, (to_name, panes)) in batches {
            self.submit_move(to, &to_name, &panes, submitter);
        }
    }

    fn submit_move(
        &mut self,
        to: SessionKey,
        to_name: &str,
        panes: &[MovedPane],
        submitter: &impl Submitter,
    ) {
        for chunk in panes.chunks(MAX_ORG_BATCH) {
            let keys: Vec<ResourceKey> = chunk.iter().map(|moved| moved.pane).collect();
            match submitter.move_resources(&keys, to) {
                Ok(operations) if !operations.is_empty() => self.track(
                    Pending::Move {
                        panes: chunk.to_vec(),
                        to_name: to_name.to_owned(),
                    },
                    operations,
                ),
                Ok(_) => {}
                Err(err) => self.fail(
                    format!("move {} terminals to {to_name}", keys.len()),
                    &err.to_string(),
                ),
            }
        }
    }

    fn submit_renames(&mut self, renames: &[PlannedRename], submitter: &impl Submitter) {
        for rename in renames {
            match submitter.rename_session(rename.session, &rename.to) {
                Ok(operation) => self.track(Pending::Rename(rename.clone()), vec![operation]),
                Err(err) => self.fail(
                    format!("rename {} to {}", rename.from, rename.to),
                    &err.to_string(),
                ),
            }
        }
    }

    fn submit_moves_back(
        &mut self,
        journal: &Journal,
        replica: &OrganizationReplica,
        submitter: &impl Submitter,
    ) {
        let mut back: BTreeMap<SessionKey, Vec<MovedPane>> = BTreeMap::new();
        for moved in &journal.moved {
            let still_there = replica.resource(&moved.pane).map(|r| r.session) == Some(moved.to);
            if still_there && replica.session(&moved.from).is_some() {
                back.entry(moved.from).or_default().push(MovedPane {
                    pane: moved.pane,
                    from: moved.to,
                    to: moved.from,
                });
            } else {
                self.skip(format!("move {} back", moved.pane), "moved or gone since");
            }
        }
        for (to, panes) in back {
            let name = replica
                .session(&to)
                .map_or_else(|| to.to_string(), |session| session.name.clone());
            self.submit_move(to, &name, &panes, submitter);
        }
    }

    fn submit_deletes(&mut self, journal: &Journal, submitter: &impl Submitter) {
        for created in &journal.created {
            match submitter.delete_session(created.key) {
                Ok(operation) => self.track(Pending::Delete(created.clone()), vec![operation]),
                Err(err) => self.fail(format!("delete {}", created.name), &err.to_string()),
            }
        }
    }

    /// Credit or report one finished entry.
    fn settle(&mut self, pending: Pending, ok: bool, last: &OrgOutcome) {
        let journal = &mut self.report.journal;
        match (pending, ok) {
            (Pending::Create(name), true) => {
                if let Some(key) = self.new_keys.get(&name) {
                    journal.created.push(CreatedSession { key: *key, name });
                }
            }
            (Pending::Move { panes, .. }, true) => journal.moved.extend(panes),
            (Pending::Rename(rename), true) => journal.renamed.push(rename),
            (Pending::Delete(created), true) => journal.created.push(created),
            (Pending::Delete(created), false) if is_not_empty(last) => self.skip(
                format!("delete {}", created.name),
                "it holds work now; left in place",
            ),
            (pending, false) => {
                let step = describe(&pending);
                self.fail(step, &outcome_text(last));
            }
        }
    }

    fn fail(&mut self, step: String, message: &str) {
        self.report.failures.push(Step {
            step,
            message: message.to_owned(),
        });
    }

    fn skip(&mut self, step: String, message: &str) {
        self.report.skipped.push(Step {
            step,
            message: message.to_owned(),
        });
    }
}

/// The renames that put `journal`'s sessions back, for sessions still
/// carrying the name the run gave them; the rest are skipped.
fn reversed_renames(
    journal: &Journal,
    replica: &OrganizationReplica,
    report: &mut Report,
) -> Vec<PlannedRename> {
    let mut back = Vec::new();
    for rename in journal.renamed.iter().rev() {
        let current = replica.session(&rename.session).map(|s| s.name.as_str());
        if current == Some(rename.to.as_str()) {
            back.push(PlannedRename {
                session: rename.session,
                from: rename.to.clone(),
                to: rename.from.clone(),
            });
        } else {
            report.skipped.push(Step {
                step: format!("rename {} back to {}", rename.to, rename.from),
                message: "changed since".to_owned(),
            });
        }
    }
    back
}

fn describe(pending: &Pending) -> String {
    match pending {
        Pending::Create(name) => format!("create {name}"),
        Pending::Move { panes, to_name } => format!("move {} terminals to {to_name}", panes.len()),
        Pending::Rename(rename) => format!("rename {} to {}", rename.from, rename.to),
        Pending::Delete(created) => format!("delete {}", created.name),
    }
}

const fn is_not_empty(outcome: &OrgOutcome) -> bool {
    matches!(outcome, OrgOutcome::Refused(Refusal::NotEmpty { .. }))
}

fn outcome_text(outcome: &OrgOutcome) -> String {
    match outcome {
        OrgOutcome::Refused(refusal) => refusal_sentence(refusal),
        OrgOutcome::Error { message, .. } => message.clone(),
        OrgOutcome::Unknown { reason } | OrgOutcome::NotSent { reason } => reason.clone(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::cell::RefCell;

    use phux_client_core::organize::{JudgeKind, NewSession, PlannedMove};
    use phux_protocol::ids::Revision;
    use phux_protocol::wire::frame::{
        Committed, OrgFilter, OrganizationPage, ResourceRecord, SessionRecord,
    };

    use super::*;

    fn skey(n: u8) -> SessionKey {
        SessionKey::new([n; 16]).expect("non-zero")
    }

    fn rkey(n: u8) -> ResourceKey {
        ResourceKey::new([n; 16]).expect("non-zero")
    }

    /// Records every submission, minting sequential operation ids.
    #[derive(Default)]
    struct Fake {
        sent: RefCell<Vec<(OperationId, String)>>,
    }

    impl Fake {
        fn mint(&self, what: String) -> OperationId {
            let mut sent = self.sent.borrow_mut();
            let n = u8::try_from(sent.len() + 1).expect("few");
            let operation = OperationId::new([n; 16]).expect("non-zero");
            sent.push((operation, what));
            operation
        }

        fn sent(&self) -> Vec<String> {
            self.sent
                .borrow()
                .iter()
                .map(|(_, what)| what.clone())
                .collect()
        }

        fn op(&self, index: usize) -> OperationId {
            self.sent.borrow()[index].0
        }
    }

    impl Submitter for Fake {
        fn submit(&self, command: Command) -> Result<OperationId, SubmitError> {
            let Command::SessionCreate { name, .. } = command else {
                panic!("only creates");
            };
            Ok(self.mint(format!("create {name}")))
        }
        fn move_resources(
            &self,
            resources: &[ResourceKey],
            to: SessionKey,
        ) -> Result<Vec<OperationId>, SubmitError> {
            Ok(vec![self.mint(format!("move {} to {to}", resources.len()))])
        }
        fn rename_session(&self, _: SessionKey, name: &str) -> Result<OperationId, SubmitError> {
            Ok(self.mint(format!("rename {name}")))
        }
        fn delete_session(&self, session: SessionKey) -> Result<OperationId, SubmitError> {
            Ok(self.mint(format!("delete {session}")))
        }
    }

    fn replica(
        resources: &[(ResourceKey, SessionKey)],
        sessions: &[(SessionKey, &str)],
    ) -> OrganizationReplica {
        let page = OrganizationPage::new(
            AuthorityId::new([9; 16]).expect("non-zero"),
            Revision::new(10),
            1,
        )
        .with_sessions(
            sessions
                .iter()
                .map(|(key, name)| SessionRecord::new(*key, *name, Revision::new(1)))
                .collect(),
        )
        .with_resources(
            resources
                .iter()
                .map(|(key, session)| ResourceRecord::new(*key, *session, Revision::new(1)))
                .collect(),
        );
        let mut replica = OrganizationReplica::new();
        replica.begin_seed(OrgFilter::All);
        replica.apply_page(page);
        replica
    }

    fn committed(session: Option<SessionKey>) -> OrgOutcome {
        let mut receipt = Committed::new(OperationId::new([200; 16]).unwrap(), Revision::new(2));
        receipt.session = session;
        OrgOutcome::Committed(Box::new(receipt))
    }

    fn plan() -> Plan {
        Plan {
            judge: JudgeKind::Jev,
            creates: vec![NewSession {
                name: "web".into(),
                project_id: "github.com/o/web".into(),
                project_name: "web".into(),
                checkout: "/src/web".into(),
            }],
            moves: vec![
                PlannedMove {
                    pane: rkey(10),
                    label: "@10".into(),
                    from: skey(1),
                    from_name: "misc".into(),
                    to: None,
                    to_name: "web".into(),
                    confidence: 0.9,
                },
                PlannedMove {
                    pane: rkey(11),
                    label: "@11".into(),
                    from: skey(1),
                    from_name: "misc".into(),
                    to: Some(skey(2)),
                    to_name: "phux".into(),
                    confidence: 0.9,
                },
            ],
            renames: Vec::new(),
            kept: Vec::new(),
        }
    }

    /// Creates first; moves only once the create's key is known; a pane
    /// moved since the plan is skipped; the journal records what landed.
    #[test]
    fn apply_creates_then_moves_into_the_created_key() {
        let replica = replica(
            &[(rkey(10), skey(1)), (rkey(11), skey(3))],
            &[(skey(1), "misc"), (skey(2), "phux"), (skey(3), "moved")],
        );
        let fake = Fake::default();
        let mut run = Sequencer::apply(plan(), replica.authority());
        assert_eq!(run.start(&replica, &fake), None);
        assert_eq!(fake.sent(), vec!["create web"]);
        let web = skey(7);
        assert_eq!(
            run.on_outcome(fake.op(0), &committed(Some(web)), &replica, &fake),
            None
        );
        assert_eq!(fake.sent().len(), 2, "{:?}", fake.sent());
        assert_eq!(fake.sent()[1], format!("move 1 to {web}"));
        let report = run
            .on_outcome(fake.op(1), &committed(None), &replica, &fake)
            .expect("done");
        assert_eq!(
            report.journal.created,
            vec![CreatedSession {
                key: web,
                name: "web".into()
            }]
        );
        assert_eq!(report.journal.moved.len(), 1);
        assert_eq!(report.journal.moved[0].to, web);
        assert_eq!(report.skipped.len(), 1, "{report:?}");
        assert!(report.failures.is_empty());
        assert_eq!(report.journal.authority, replica.authority());
    }

    /// Undo moves back and renames back first, then deletes; a delete
    /// refused `NOT_EMPTY` is skipped, not failed.
    #[test]
    fn undo_moves_back_then_deletes() {
        let replica = replica(
            &[(rkey(10), skey(7))],
            &[(skey(1), "misc"), (skey(7), "web")],
        );
        let mut journal = Journal::new();
        journal.authority = replica.authority();
        journal.created.push(CreatedSession {
            key: skey(7),
            name: "web".into(),
        });
        journal.moved.push(MovedPane {
            pane: rkey(10),
            from: skey(1),
            to: skey(7),
        });
        let fake = Fake::default();
        let mut run = Sequencer::undo(journal);
        assert_eq!(run.start(&replica, &fake), None);
        assert_eq!(fake.sent().len(), 1);
        assert!(fake.sent()[0].starts_with("move 1 to"));
        assert_eq!(
            run.on_outcome(fake.op(0), &committed(None), &replica, &fake),
            None
        );
        assert!(fake.sent()[1].starts_with("delete"));
        let refused = OrgOutcome::Refused(Refusal::NotEmpty { members: 1 });
        let report = run
            .on_outcome(fake.op(1), &refused, &replica, &fake)
            .expect("done");
        assert_eq!(report.journal.moved.len(), 1);
        assert!(report.failures.is_empty(), "{report:?}");
        assert_eq!(report.skipped.len(), 1);
    }

    /// Another server's record is refused before anything is sent.
    #[test]
    fn undo_refuses_another_servers_record() {
        let replica = replica(&[], &[]);
        let mut journal = Journal::new();
        journal.authority = Some(AuthorityId::new([3; 16]).expect("non-zero"));
        let fake = Fake::default();
        let report = Sequencer::undo(journal)
            .start(&replica, &fake)
            .expect("done");
        assert_eq!(report.failures.len(), 1);
        assert!(fake.sent().is_empty());
    }
}
