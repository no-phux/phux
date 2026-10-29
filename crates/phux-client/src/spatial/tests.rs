use super::*;
use crate::pane_move::PaneMoveError;
use crate::spatial::plan::exactly_one_local;
use phux_protocol::ids::{SatelliteHost, WindowId};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

fn snapshot() -> SessionSnapshot {
    SessionSnapshot::new(SessionId::new(1), WindowId::new(10), ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "one"),
            SessionInfo::new(SessionId::new(2), "two"),
        ])
        .with_windows(vec![
            WindowInfo::new(WindowId::new(10), SessionId::new(1), "a"),
            WindowInfo::new(WindowId::new(20), SessionId::new(2), "b"),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), WindowId::new(10), 80, 24),
            ResourceInfo::new(ResourceId::local(2), WindowId::new(10), 80, 24),
            ResourceInfo::new(ResourceId::local(3), WindowId::new(20), 80, 24),
        ])
}

fn local(plan: PlanKind) -> Plan {
    match plan {
        PlanKind::Local(plan) => plan,
        PlanKind::CrossMove(other) => panic!("expected a local plan, got {other:?}"),
    }
}

async fn plan(op: SpatialOp) -> Result<PlanKind, SpatialRefusal> {
    let path = Path::new("/unused-for-local-selectors");
    let selectors = op.parse_selectors().unwrap();
    build_plan(path, &snapshot(), op, selectors).await
}

#[tokio::test]
async fn cross_session_move_takes_the_shared_l1_path() {
    let op = SpatialOp::Move {
        source: "@1".to_owned(),
        target: "@3".to_owned(),
        direction: Direction::Horizontal,
        ratio: 0.5,
        projection: Vec::new(),
    };
    match plan(op).await.unwrap() {
        PlanKind::CrossMove(plan) => {
            assert_eq!(plan.source, ResourceId::local(1));
            assert_eq!(plan.target, ResourceId::local(3));
            assert_eq!(plan.outcome.document["cross_session"], true);
        }
        PlanKind::Local(other) => panic!("expected a cross-session plan, got {other:?}"),
    }

    // Insert and swap keep the same-session requirement.
    let op = SpatialOp::Insert {
        target: "@1".to_owned(),
        new_pane: "@3".to_owned(),
        direction: Direction::Horizontal,
        ratio: 0.5,
        projection: Vec::new(),
    };
    assert_eq!(plan(op).await.unwrap_err().code, "cross_session");
}

/// `%name` (ADR-0075) reaches a spatial edit through the agent resolver:
/// a named pane plans like its `@N`, and a refusal keeps its own code and
/// exit status rather than collapsing to `selector_miss`.
#[tokio::test]
async fn percent_name_resolves_through_the_agent_resolver() {
    use crate::agent_meta::{AgentRecord, RESOURCE_AGENT_KEY};
    use crate::testkit::{self, ScriptSpec};
    use phux_protocol::wire::frame::Scope;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("spatial.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(testkit::serve_every(listener, || {
        let record = |name: &str, kind: Option<&str>| {
            AgentRecord {
                name: name.to_owned(),
                kind: kind.map(str::to_owned),
                ..AgentRecord::default()
            }
            .encode()
        };
        ScriptSpec::new()
            .stored_metadata(
                Scope::Resource(ResourceId::local(1)),
                RESOURCE_AGENT_KEY,
                record("build", None),
            )
            .stored_metadata(
                Scope::Resource(ResourceId::local(2)),
                RESOURCE_AGENT_KEY,
                record("claude", Some("claude")),
            )
    }));
    let swap = |first: &str, second: &str| SpatialOp::Swap {
        first: first.to_owned(),
        second: second.to_owned(),
        projection: Vec::new(),
    };
    let plan_on = |op: SpatialOp| {
        let socket = socket.clone();
        async move {
            let selectors = op.parse_selectors().unwrap();
            build_plan(&socket, &snapshot(), op, selectors).await
        }
    };

    let named = local(plan_on(swap("%build", "@2")).await.unwrap());
    let by_id = local(plan_on(swap("@1", "@2")).await.unwrap());
    assert_eq!(named.mutation, by_id.mutation);

    let constant = plan_on(swap("%claude", "@1")).await.unwrap_err();
    assert_eq!(
        (constant.code, constant.exit_code),
        ("invalid_agent_name", 2)
    );
    let unknown = plan_on(swap("@1", "%ghost")).await.unwrap_err();
    assert_eq!((unknown.code, unknown.exit_code), ("no_such_target", 1));
    assert!(
        unknown.message.starts_with("second selector:"),
        "{unknown:?}"
    );
    server.abort();
}

/// A destination placement that fails after ownership moved is
/// `destination_layout_failed`, exit 1, naming the failure and rollback.
#[test]
fn a_failed_destination_placement_is_a_post_ownership_refusal() {
    let SpatialError::Refused(refusal) = move_error(PaneMoveError::DestinationLayout {
        error: "pane is not in this session layout".to_owned(),
        rollback: "the move was undone",
    }) else {
        panic!("a destination failure is a refusal");
    };
    assert_eq!(refusal.code, codes::DESTINATION_LAYOUT_FAILED);
    assert_eq!(refusal.exit_code, 1);
    assert!(!refusal.remedy.is_empty());
    assert!(
        refusal.message.contains("the move was undone"),
        "{refusal:?}"
    );
}

#[test]
fn ratio_must_be_finite_and_strictly_inside_unit_interval() {
    assert!(validate_ratio(0.3).is_ok());
    for ratio in [0.0, 1.0, -0.1, 1.1, f32::NAN, f32::INFINITY] {
        assert_eq!(validate_ratio(ratio).unwrap_err().code, "invalid_ratio");
    }
}

#[test]
fn selectors_must_resolve_to_exactly_one_local_terminal() {
    assert_eq!(
        exactly_one_local("target", &[]).unwrap_err().code,
        "selector_miss"
    );
    assert_eq!(
        exactly_one_local("target", &[ResourceId::local(1), ResourceId::local(2)])
            .unwrap_err()
            .code,
        "selector_not_single"
    );
    let satellite = ResourceId::satellite(SatelliteHost::new("edge"), 7);
    assert_eq!(
        exactly_one_local("target", &[satellite]).unwrap_err().code,
        "satellite_target"
    );
    assert_eq!(
        exactly_one_local("target", &[ResourceId::local(7)]).unwrap(),
        ResourceId::local(7)
    );
}

#[tokio::test]
async fn plans_map_arguments_to_all_layout_mutations() {
    let plan_insert = local(
        plan(SpatialOp::Insert {
            target: "@1".to_owned(),
            new_pane: "@2".to_owned(),
            direction: Direction::Vertical,
            ratio: 0.3,
            projection: Vec::new(),
        })
        .await
        .unwrap(),
    );
    assert!(matches!(
        plan_insert.mutation,
        LayoutMutation::Split {
            target,
            new_pane,
            dir: SplitDir::Horizontal,
            ratio,
        } if target == ResourceId::local(1)
            && new_pane == ResourceId::local(2)
            && (ratio - 0.3).abs() < f32::EPSILON
    ));
    assert_eq!(plan_insert.outcome.document["schema_version"], 1);
    assert_eq!(plan_insert.outcome.document["operation"], "insert-pane");
    assert_eq!(
        plan_insert.outcome.document["direction"], "vertical",
        "JSON retains the user-facing divider label"
    );

    let plan_move = local(
        plan(SpatialOp::Move {
            source: "@1".to_owned(),
            target: "@2".to_owned(),
            direction: Direction::Horizontal,
            ratio: 0.5,
            projection: Vec::new(),
        })
        .await
        .unwrap(),
    );
    assert!(matches!(
        plan_move.mutation,
        LayoutMutation::Move {
            dir: SplitDir::Vertical,
            ..
        }
    ));
    assert_eq!(
        plan_move.outcome.document["direction"], "horizontal",
        "JSON retains the user-facing divider label"
    );

    let plan_swap = local(
        plan(SpatialOp::Swap {
            first: "@1".to_owned(),
            second: "@2".to_owned(),
            projection: Vec::new(),
        })
        .await
        .unwrap(),
    );
    assert!(matches!(plan_swap.mutation, LayoutMutation::Swap { .. }));

    let same = SpatialOp::Swap {
        first: "@1".to_owned(),
        second: "@1".to_owned(),
        projection: Vec::new(),
    };
    assert_eq!(plan(same).await.unwrap_err().code, "same_pane");
}
