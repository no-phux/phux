use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};

use super::{Prior, retract_hook, spawn_agent_state_drain, state_change_hook};
use crate::hooks::AGENT_STATE_UNKNOWN;
use crate::state::SharedState;
use phux_resource::agent_asked::AskedPayload;
use phux_resource::agent_record::AgentRecordJson;
use phux_terminal::agent_detect::{AgentDetectEvent, AgentReport, DetectedState};

fn report_of(kind: &str, state: DetectedState) -> AgentDetectEvent {
    AgentDetectEvent::State(AgentReport {
        kind: kind.to_owned(),
        name: kind.to_owned(),
        state,
    })
}

fn claude(state: DetectedState) -> AgentDetectEvent {
    report_of("claude", state)
}

fn reidentified(kind: &str) -> AgentDetectEvent {
    AgentDetectEvent::Reidentified {
        kind: kind.to_owned(),
        name: kind.to_owned(),
    }
}

/// A pane at wire id 1, optionally carrying a record its human wrote.
fn pane(declared: Option<&[u8]>) -> (SharedState, WireResourceId) {
    let state = SharedState::new();
    let terminal = WireResourceId::new(1);
    if let Some(bytes) = declared {
        state.with_mut(|s| {
            s.agent_records_mut().note_explicit_set(&terminal, bytes);
            s.metadata_set(
                &Scope::Resource(terminal.clone()),
                RESOURCE_AGENT_KEY,
                bytes.to_vec(),
            );
        });
    }
    (state, terminal)
}

/// Drive the real drain task to quiescence over `events`.
async fn drain(state: &SharedState, terminal: &WireResourceId, events: Vec<AgentDetectEvent>) {
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    spawn_agent_state_drain(state.clone(), terminal.clone(), rx);
    for event in events {
        tx.send(event).await.expect("drain is alive");
    }
    drop(tx);
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

fn raw(state: &SharedState, terminal: &WireResourceId) -> Option<Vec<u8>> {
    let scope = Scope::Resource(terminal.clone());
    state.with(|s| s.metadata().get(&scope, RESOURCE_AGENT_KEY))
}

fn stored(state: &SharedState, terminal: &WireResourceId) -> Option<AgentRecordJson> {
    raw(state, terminal).and_then(|bytes| AgentRecordJson::decode(&bytes))
}

async fn local(test: impl std::future::Future<Output = ()>) {
    tokio::task::LocalSet::new().run_until(test).await;
}

#[test]
fn hooks_fire_only_on_real_edges() {
    let terminal = WireResourceId::local(1);
    let ctx = |event: &crate::hooks::HookEvent, key: &str| event.context.get(key).cloned();
    let was = |state: &str| Some(Prior(Some(state.to_owned())));

    assert!(
        state_change_hook(&terminal, "claude", "", None, "working").is_none(),
        "no hook is owed while hooks are off"
    );
    let first =
        state_change_hook(&terminal, "claude", "", Some(Prior(None)), "working").expect("edge");
    assert_eq!(first.name, crate::hooks::AGENT_STATE_CHANGED);
    assert_eq!(
        ctx(&first, "from"),
        None,
        "a first sighting has no prior state"
    );
    assert_eq!(ctx(&first, "agent-name"), None, "an empty name is omitted");
    let blocked =
        state_change_hook(&terminal, "claude", "rev", was("working"), "blocked").expect("edge");
    assert_eq!(ctx(&blocked, "from").as_deref(), Some("working"));
    assert_eq!(ctx(&blocked, "to").as_deref(), Some("blocked"));
    assert!(state_change_hook(&terminal, "claude", "claude", was("idle"), "idle").is_none());

    let retract = retract_hook(&terminal, was("working")).expect("a retract is an edge");
    assert_eq!(ctx(&retract, "to").as_deref(), Some(AGENT_STATE_UNKNOWN));
    assert!(retract_hook(&terminal, was(AGENT_STATE_UNKNOWN)).is_none());
}

/// One drain run per row over a pane that may carry a human's record:
/// the resulting `(kind, name, state, session)`, or `None` when no record
/// remains.
///
/// - An identity-only record is not a declaration: the detector fills in
///   `state`, and a retract withdraws it without deleting the name.
/// - A record the detector wrote alone is deleted by a retract.
/// - I2: a new occupant lands on `unknown` in one write; a declaration is
///   withdrawn rather than corrected; no record means nothing is written.
/// - I1: a dropped `Reidentified` is healed by the next `State` write.
/// - A custom kind the detector cannot derive still gets its state, and
///   reasserting a kind never drags a human's name with it.
#[tokio::test(flavor = "current_thread")]
async fn detector_writes_respect_human_records() {
    type Expect<'a> = Option<(Option<&'a str>, &'a str, &'a str, Option<&'a str>)>;
    type Case<'a> = (Option<&'a [u8]>, Vec<AgentDetectEvent>, Expect<'a>);
    let identity = br#"{"name":"reviewer","kind":"claude","session":"fleet-7"}"#.as_slice();
    let declared = br#"{"name":"me","kind":"claude","state":"working"}"#.as_slice();
    let custom = br#"{"name":"reviewer","kind":"my-agent","session":"fleet-7"}"#.as_slice();
    let nameless = br#"{"name":"reviewer","session":"fleet-7"}"#.as_slice();
    let cases: Vec<Case<'_>> = vec![
        (
            Some(identity),
            vec![claude(DetectedState::Working), AgentDetectEvent::Retract],
            Some((
                Some("claude"),
                "reviewer",
                AGENT_STATE_UNKNOWN,
                Some("fleet-7"),
            )),
        ),
        (
            None,
            vec![claude(DetectedState::Working), AgentDetectEvent::Retract],
            None,
        ),
        (
            None,
            vec![claude(DetectedState::Working), reidentified("codex")],
            Some((Some("codex"), "codex", AGENT_STATE_UNKNOWN, None)),
        ),
        (
            None,
            vec![
                claude(DetectedState::Working),
                report_of("codex", DetectedState::Blocked),
            ],
            Some((Some("codex"), "codex", "blocked", None)),
        ),
        (
            Some(declared),
            vec![reidentified("codex")],
            Some((Some("claude"), "me", AGENT_STATE_UNKNOWN, None)),
        ),
        (None, vec![reidentified("codex")], None),
        (
            Some(custom),
            vec![claude(DetectedState::Blocked)],
            Some((Some("my-agent"), "reviewer", "blocked", Some("fleet-7"))),
        ),
        (
            Some(nameless),
            vec![
                claude(DetectedState::Working),
                report_of("codex", DetectedState::Idle),
            ],
            Some((Some("codex"), "reviewer", "idle", Some("fleet-7"))),
        ),
    ];
    for (row, (record, events, expect)) in cases.into_iter().enumerate() {
        local(async {
            let (state, terminal) = pane(record);
            drain(&state, &terminal, events).await;
            let got = stored(&state, &terminal);
            let got = got.as_ref().map(|r| {
                (
                    r.kind.as_deref(),
                    r.name.as_str(),
                    r.state.as_str(),
                    r.session.as_deref(),
                )
            });
            assert_eq!(got, expect, "row {row}");
        })
        .await;
    }
}

/// A declared state outranks derivations (L3 §3.7), but a confirmed
/// departure withdraws it to `unknown`, never deletes it; afterwards the
/// detector's writes land again (phux-w7z2.13: a `kill -9` must not pin
/// the pane at `working`).
#[tokio::test(flavor = "current_thread")]
async fn a_retract_withdraws_a_declared_state_but_never_deletes_it() {
    local(async {
        let (state, terminal) = pane(Some(
            br#"{"name":"me","kind":"claude","state":"working","attention":"high"}"#,
        ));
        drain(&state, &terminal, vec![claude(DetectedState::Idle)]).await;
        assert_eq!(
            stored(&state, &terminal).expect("declared").state,
            "working"
        );

        drain(&state, &terminal, vec![AgentDetectEvent::Retract]).await;
        let record = stored(&state, &terminal).expect("withdrawing is not deleting");
        assert_eq!(record.state, AGENT_STATE_UNKNOWN);
        assert_eq!(record.name, "me");
        assert_eq!(record.kind.as_deref(), Some("claude"));
        assert_eq!(record.attention, None);

        drain(&state, &terminal, vec![claude(DetectedState::Working)]).await;
        let after = stored(&state, &terminal).expect("still there");
        assert_eq!(after.state, "working", "the detector resumed");
        assert_eq!(after.name, "me");
    })
    .await;
}

/// phux-w7z2.45: a shim pane declared `kind: claude` now running codex
/// keeps the declared kind (L3 §3.7) and withholds codex's state, and
/// every later contradicted tick writes nothing.
#[tokio::test(flavor = "current_thread")]
async fn a_shim_pane_never_takes_a_state_from_an_occupant_its_kind_denies() {
    local(async {
        let (state, terminal) = pane(Some(br#"{"name":"claude","kind":"claude"}"#));
        drain(
            &state,
            &terminal,
            vec![
                claude(DetectedState::Working),
                reidentified("codex"),
                report_of("codex", DetectedState::Working),
            ],
        )
        .await;
        let record = stored(&state, &terminal).expect("still there");
        assert_eq!(record.kind.as_deref(), Some("claude"));
        assert_eq!(record.name, "claude");
        assert_eq!(record.state, AGENT_STATE_UNKNOWN);

        let first = raw(&state, &terminal);
        let churn = [
            DetectedState::Blocked,
            DetectedState::Idle,
            DetectedState::Working,
            DetectedState::Done,
        ];
        let ticks = (0..20)
            .map(|i| report_of("codex", churn[i % churn.len()]))
            .collect();
        drain(&state, &terminal, ticks).await;
        assert_eq!(raw(&state, &terminal), first, "withholding is free");
    })
    .await;
}

/// Level-triggered: the state resumes once the pane runs the declared
/// kind again, and clearing the declaration hands the kind back.
#[tokio::test(flavor = "current_thread")]
async fn a_contradiction_heals_on_the_declared_kind_or_a_cleared_declaration() {
    local(async {
        let (state, terminal) = pane(Some(br#"{"name":"claude","kind":"claude"}"#));
        drain(
            &state,
            &terminal,
            vec![report_of("codex", DetectedState::Working)],
        )
        .await;
        assert_eq!(
            stored(&state, &terminal).expect("written").state,
            AGENT_STATE_UNKNOWN
        );
        drain(&state, &terminal, vec![claude(DetectedState::Blocked)]).await;
        assert_eq!(stored(&state, &terminal).expect("written").state, "blocked");

        drain(
            &state,
            &terminal,
            vec![report_of("codex", DetectedState::Working)],
        )
        .await;
        let scope = Scope::Resource(terminal.clone());
        state.with_mut(|s| {
            s.agent_records_mut().note_explicit_delete(&terminal);
            s.metadata_delete(&scope, RESOURCE_AGENT_KEY);
        });
        drain(
            &state,
            &terminal,
            vec![report_of("codex", DetectedState::Working)],
        )
        .await;
        let record = stored(&state, &terminal).expect("rewritten");
        assert_eq!(record.kind.as_deref(), Some("codex"));
        assert_eq!(record.state, "working");
    })
    .await;
}

/// phux-uaon: an identity-only SET that reached the store but not the
/// arbiter still survives the next detector write.
#[tokio::test(flavor = "current_thread")]
async fn a_subsequent_detector_write_merges_over_an_identity_only_set() {
    local(async {
        let (state, terminal) = pane(None);
        drain(&state, &terminal, vec![claude(DetectedState::Blocked)]).await;
        let scope = Scope::Resource(terminal.clone());
        state.with_mut(|s| {
            s.metadata_set(
                &scope,
                RESOURCE_AGENT_KEY,
                br#"{"name":"reviewer","session":"fleet-7"}"#.to_vec(),
            );
        });
        drain(&state, &terminal, vec![claude(DetectedState::Blocked)]).await;
        let record = stored(&state, &terminal).expect("merged");
        assert_eq!(record.name, "reviewer");
        assert_eq!(record.session.as_deref(), Some("fleet-7"));
        assert_eq!(record.state, "blocked");
        assert_eq!(record.kind.as_deref(), Some("claude"));
    })
    .await;
}

/// ADR-0036 tier 2: a `phux-ask` marker lands in the same ledger a
/// `REPORT_ASKED` hook writes to, and clearing it retracts the ask.
#[tokio::test(flavor = "current_thread")]
async fn an_ask_sentinel_lands_in_the_ledger_the_hook_shares() {
    local(async {
        let state = SharedState::new();
        let (pane, terminal) = state.with_mut(|s| {
            let (_session, pane) = s.seed_session("demo");
            (pane, s.intern_terminal_wire(pane))
        });
        let ask = AskedPayload {
            id: "q1".to_owned(),
            question: "Deploy to prod?".to_owned(),
            suggestions: vec!["Yes".to_owned(), "No".to_owned()],
            elapsed_seconds: None,
        };
        drain(
            &state,
            &terminal,
            vec![AgentDetectEvent::AskSentinel(Some(ask))],
        )
        .await;
        assert_eq!(
            state.with(|s| s.current_agent_asked(pane).map(|p| p.id.clone())),
            Some("q1".to_owned()),
        );
        drain(&state, &terminal, vec![AgentDetectEvent::AskSentinel(None)]).await;
        assert!(state.with(|s| s.current_agent_asked(pane).is_none()));
    })
    .await;
}
