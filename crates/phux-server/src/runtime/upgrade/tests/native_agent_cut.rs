#![allow(clippy::unwrap_used, reason = "tests")]

use std::collections::{BTreeMap, HashMap, VecDeque};

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::native_agent::{NativeCapabilities, NativeView, SpawnNativeAgent};

use super::super::*;
use super::support::*;
use crate::state::native_agent::NativeBinding;

/// The wire id of a native agent no engine answers for.
const GHOST: u32 = 9_999;

/// Bind an open native controller to `GHOST`, under `parent`. No engine
/// stands behind it, so no upgrade can cut its stream.
fn bind_ghost(s: &mut crate::state::ServerState, parent: WireResourceId) -> WireResourceId {
    let agent = WireResourceId::local(GHOST);
    let binding = NativeBinding {
        launch: SpawnNativeAgent {
            session: "main".into(),
            backend: "acp".into(),
            provider: "omp".into(),
            command: vec!["provider-never-started".into()],
            cwd: None,
            env: BTreeMap::new(),
            native_id: None,
            guardian: None,
            guardian_key: None,
            guardian_expected: None,
        },
        agent: agent.clone(),
        parent,
        activation: "activation-A".into(),
        proof: "private-test-proof".into(),
        credential: "delegated:test".into(),
        host_pid: None,
        generation: 0,
        view: NativeView {
            activation: "activation-A".into(),
            backend: "acp".into(),
            capabilities: NativeCapabilities::default(),
            connected: false,
            turn_id: None,
            provider_session_id: None,
            questions: vec![],
        },
        closed: false,
        closure_committed: false,
        publication_committed: 0,
        evidence_seq: 0,
        operations: BTreeMap::new(),
        pending: VecDeque::new(),
        answered: BTreeMap::new(),
        publications: VecDeque::new(),
        publication_high_water: 0,
    };
    s.native_agents_mut()
        .bindings
        .insert(agent.clone(), binding);
    agent
}

/// An SDK-owned stream is part of a durable provider session, so an open
/// native binding the cut could not carry refuses the whole upgrade, and
/// the refusal reopens every stream the cut sealed and the native broker.
#[tokio::test(flavor = "current_thread")]
async fn an_uncarried_native_agent_refuses_the_upgrade_and_unseals_the_streams() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (pane_wire, _, append) = state.with_mut(old_image_with_agent_sessions);
            append_records(&append, "{\"type\":\"prompt\"}").await;
            let listener = tempfile::tempfile().unwrap();
            let dir = tempfile::tempdir().unwrap();
            let ghost = state.with_mut(|s| {
                s.set_upgrade_context(
                    listener.as_raw_fd(),
                    dir.path().join("phux.sock"),
                    RuntimeFlags::default(),
                );
                bind_ghost(s, pane_wire)
            });

            let Err(UpgradeError::NativeAgentHandoff { agent }) = prepare_upgrade(&state).await
            else {
                panic!("an open native binding without a cut must refuse the upgrade");
            };
            assert_eq!(agent, ghost);

            assert!(
                !state.with(|s| s.native_agents().sealed),
                "the refusal unseals the native broker"
            );
            let next = append_records(&append, "{\"type\":\"stop\"}").await;
            assert_eq!(
                next.first_seq, 2,
                "the cut stream reopens where it was sealed"
            );
        })
        .await;
}

/// A changed tree is checked before any kind writes its cut, so a tree
/// change wins over an uncarried native agent: the retry the operator is
/// told to make may well carry it.
#[tokio::test(flavor = "current_thread")]
async fn a_changed_tree_outranks_an_uncarried_native_agent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (pane_wire, _, _) = state.with_mut(old_image_with_agent_sessions);
            let ghost = state.with_mut(|s| bind_ghost(s, pane_wire));
            let context = capture_upgrade_context(&state).unwrap();
            let cuts = [AgentSessionUpgrade::carrying(HashMap::new())];
            let reassemble = || {
                reassemble_unchanged_tree(
                    &state,
                    context.listener_fd,
                    context.listener_fd,
                    &context.tree_identity,
                    &cuts,
                )
            };

            let Err(UpgradeError::NativeAgentHandoff { agent }) = reassemble() else {
                panic!("the unchanged tree reaches the native agent's refusal");
            };
            assert_eq!(agent, ghost);

            state.with_mut(|s| {
                let _ = s.seed_empty_session("late").expect("seed");
            });
            assert!(matches!(reassemble(), Err(UpgradeError::TreeChanged)));
        })
        .await;
}
