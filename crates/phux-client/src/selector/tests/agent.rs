use super::*;
use crate::agent_meta::{AgentMetaState, AgentRecord};

// ---- ADR-0075: `%name` agent addressing -----------------------------

fn record(name: &str, kind: Option<&str>, state: AgentMetaState) -> AgentRecord {
    AgentRecord {
        name: name.to_owned(),
        kind: kind.map(str::to_owned),
        state,
        attention: None,
        session: None,
    }
}

fn index_of(entries: &[(u32, AgentRecord)], complete: bool) -> AgentIndex {
    let map: std::collections::HashMap<_, _> = entries
        .iter()
        .map(|(id, rec)| (ResourceId::local(*id), rec.clone()))
        .collect();
    if complete {
        AgentIndex::complete(map)
    } else {
        AgentIndex::partial(map)
    }
}

/// ADR-0075 point 4: `phux agent list` says which names `%` cannot
/// address, with the code `%name` would refuse with.
#[test]
fn agent_address_says_which_listed_names_percent_cannot_reach() {
    let snap = fixture();
    let index = index_of(
        &[
            (100, record("build", None, AgentMetaState::Working)),
            (101, record("Code Review", None, AgentMetaState::Idle)),
            (102, record("twin", None, AgentMetaState::Idle)),
            (200, record("twin", None, AgentMetaState::Idle)),
        ],
        true,
    );
    let address = |id| agent_address(&ResourceId::local(id), &snap, &index);
    assert_eq!(address(100), Some(Ok("%build".to_owned())));
    assert_eq!(address(101), Some(Err("invalid_agent_name")));
    assert_eq!(address(102), Some(Err("selector_not_single")));
    assert_eq!(address(200), Some(Err("selector_not_single")));
    assert_eq!(address(999), None, "no record, nothing to address");

    let constant = index_of(
        &[(100, record("claude", Some("claude"), AgentMetaState::Idle))],
        true,
    );
    assert_eq!(
        agent_address(&ResourceId::local(100), &snap, &constant),
        Some(Ok("%claude".to_owned())),
        "the server gives each pane of a kind its own name, so a kind name addresses",
    );

    let partial = index_of(
        &[(100, record("build", None, AgentMetaState::Working))],
        false,
    );
    assert_eq!(
        agent_address(&ResourceId::local(100), &snap, &partial),
        Some(Err("partial_view")),
    );
}

/// Every refusal carries the contract code and remedy each surface (CLI,
/// spatial, MCP) reports it with.
#[test]
fn agent_resolve_errors_carry_their_contract_codes() {
    let t = ResourceId::local(1);
    for (err, code, exit) in [
        (
            AgentResolveError::Unknown { name: "a".into() },
            "no_such_target",
            1,
        ),
        (
            AgentResolveError::Ambiguous {
                name: "a".into(),
                candidates: vec![],
            },
            "selector_not_single",
            2,
        ),
        (
            AgentResolveError::AmbiguousSession {
                name: "a".into(),
                terminal: t.clone(),
                candidates: vec![],
            },
            "selector_not_single",
            2,
        ),
        (
            AgentResolveError::Withdrawn {
                name: "a".into(),
                terminal: t,
            },
            "agent_withdrawn",
            2,
        ),
        (
            AgentResolveError::PartialIndex {
                name: "a".into(),
                matched: vec![],
            },
            "partial_view",
            3,
        ),
    ] {
        assert_eq!(err.code(), code, "{err:?}");
        assert_eq!(err.exit_code(), exit, "{err:?}");
        assert!(!err.remedy().is_empty(), "{err:?}");
    }
}

/// A miss is exit 1, an ambiguity exit 2 (candidates in snapshot order),
/// and a truncated index exit 3 even for a single or empty match.
#[test]
fn resolve_agent_refuses_a_miss_an_ambiguity_and_a_partial_index_distinctly() {
    let snap = fixture();

    let complete = index_of(
        &[(101, record("build", None, AgentMetaState::Working))],
        true,
    );
    let miss = resolve_agent("ghost", &snap, &complete).unwrap_err();
    assert_eq!(
        miss,
        AgentResolveError::Unknown {
            name: "ghost".to_owned()
        }
    );
    assert_eq!(miss.exit_code(), 1);

    let shared = index_of(
        &[
            (200, record("build", None, AgentMetaState::Idle)),
            (100, record("build", None, AgentMetaState::Working)),
        ],
        true,
    );
    let ambiguous = resolve_agent("build", &snap, &shared).unwrap_err();
    assert_eq!(
        ambiguous,
        AgentResolveError::Ambiguous {
            name: "build".to_owned(),
            candidates: ids(&[100, 200]),
        }
    );
    assert_eq!(ambiguous.exit_code(), 2);
    let rendered = ambiguous.to_string();
    assert!(
        rendered.contains("@100") && rendered.contains("@200"),
        "{rendered}"
    );

    let truncated = index_of(
        &[(101, record("build", None, AgentMetaState::Working))],
        false,
    );
    let partial = resolve_agent("build", &snap, &truncated).unwrap_err();
    assert_eq!(
        partial,
        AgentResolveError::PartialIndex {
            name: "build".to_owned(),
            matched: ids(&[101]),
        }
    );
    assert_eq!(partial.exit_code(), 3);
    let nothing = resolve_agent("ghost", &snap, &truncated).unwrap_err();
    assert_eq!(nothing.exit_code(), 3);
    // The default index is partial, so it fails closed the same way.
    let default = resolve_agent("build", &snap, &AgentIndex::default()).unwrap_err();
    assert_eq!(default.exit_code(), 3);
}

/// A name equal to its kind resolves like a chosen one: the server names
/// each pane of a kind uniquely (`claude`, `claude-2`), and records that
/// still share a name (an older server) are ambiguous, never guessed.
#[test]
fn resolve_agent_reaches_a_kind_named_pane_and_refuses_a_shared_one() {
    let snap = fixture();
    let fleet = index_of(
        &[
            (
                101,
                record("claude", Some("claude"), AgentMetaState::Working),
            ),
            (
                102,
                record("claude-2", Some("claude"), AgentMetaState::Idle),
            ),
        ],
        true,
    );
    assert_eq!(
        resolve_agent("claude", &snap, &fleet).unwrap().terminal,
        ResourceId::local(101)
    );
    assert_eq!(
        resolve_agent("claude-2", &snap, &fleet).unwrap().terminal,
        ResourceId::local(102)
    );

    let shared = index_of(
        &[
            (
                100,
                record("claude", Some("claude"), AgentMetaState::Working),
            ),
            (102, record("claude", Some("claude"), AgentMetaState::Idle)),
        ],
        true,
    );
    let err = resolve_agent("claude", &snap, &shared).unwrap_err();
    assert!(
        matches!(err, AgentResolveError::Ambiguous { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("@102"));
}

/// The write guard is the withdrawn shape (a `kind` and `state:
/// unknown`), not bare `state == unknown`; read verbs skip the gate.
#[test]
fn input_verbs_refuse_only_the_withdrawn_shape() {
    let snap = fixture();
    let withdrawn = index_of(
        &[(
            101,
            record("build", Some("claude"), AgentMetaState::Unknown),
        )],
        true,
    );
    assert_eq!(
        resolve_agent("build", &snap, &withdrawn).unwrap().terminal,
        ResourceId::local(101),
    );
    let err = resolve_agent_for_input("build", &snap, &withdrawn).unwrap_err();
    assert_eq!(
        err,
        AgentResolveError::Withdrawn {
            name: "build".to_owned(),
            terminal: ResourceId::local(101),
        }
    );
    assert_eq!(err.exit_code(), 2);

    let identity_only = index_of(
        &[(101, record("build", None, AgentMetaState::Unknown))],
        true,
    );
    assert_eq!(
        resolve_agent_for_input("build", &snap, &identity_only)
            .unwrap()
            .terminal,
        ResourceId::local(101)
    );
    assert!(!is_withdrawn_agent_record(&record(
        "build",
        Some("claude"),
        AgentMetaState::Idle
    )));
}

/// `%name` must never reach `pick_target_pane`: the set-valued seam
/// yields nothing for it, and it is never a whole-session teardown.
#[test]
fn the_shared_set_valued_seam_never_narrows_an_agent_selector() {
    let snap = fixture();
    let sel = parse("%build").unwrap();
    assert!(resolve(&sel, &snap).is_empty());
    assert_eq!(
        pick_target_pane(&resolve(&sel, &snap), &snap.focused_resource),
        None
    );
    assert_eq!(whole_session_name(&sel, &snap), None);
}

// ---- ADR-0102 / ADR-0103: resource kinds in the selector -------------

/// Pane 101 hosts agent session @901; pane 102 hosts @902 and @903.
fn kinded_fixture() -> SessionSnapshot {
    use phux_protocol::ids::ResourceKind;
    let mut snap = fixture();
    for (session, parent) in [(901, 101), (902, 102), (903, 102)] {
        snap.resources.push(
            ResourceInfo::new(ResourceId::local(session), WindowId::new(11), 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(ResourceId::local(parent))),
        );
    }
    snap
}

/// Set-valued selectors see Terminal-kind resources only (a pane index
/// counts panes); `@N` resolves any kind.
#[test]
fn set_valued_selectors_resolve_terminal_kind_only() {
    let snap = kinded_fixture();
    for (raw, expected) in [
        ("work", ids(&[100, 101, 102])),
        ("work:1", ids(&[101, 102])),
        ("work:1.1", ids(&[102])),
        ("work:1.2", vec![]),
        ("@901", ids(&[901])),
    ] {
        assert_eq!(resolve(&parse(raw).unwrap(), &snap), expected, "{raw}");
    }
    let mut tags = TagIndex::new();
    tags.insert(ResourceId::local(901), vec!["build".to_owned()]);
    tags.insert(ResourceId::local(101), vec!["build".to_owned()]);
    assert_eq!(
        resolve_with_tags(&parse("#build").unwrap(), &snap, &tags),
        ids(&[101]),
        "a tag on a session resource is not a pane match"
    );
}

/// `%name` carries the Terminal's unique live session and refuses when
/// it has several.
#[test]
fn resolve_agent_carries_the_unique_session_child_and_refuses_several() {
    let snap = kinded_fixture();
    let index = index_of(
        &[
            (
                101,
                record("reviewer", Some("claude"), AgentMetaState::Working),
            ),
            (102, record("builder", Some("claude"), AgentMetaState::Idle)),
            (100, record("solo", None, AgentMetaState::Idle)),
        ],
        true,
    );
    assert_eq!(
        resolve_agent("reviewer", &snap, &index).unwrap(),
        AgentTarget {
            terminal: ResourceId::local(101),
            session: Some(ResourceId::local(901)),
        }
    );
    assert_eq!(
        resolve_agent("solo", &snap, &index).unwrap(),
        AgentTarget {
            terminal: ResourceId::local(100),
            session: None,
        },
    );
    let err = resolve_agent("builder", &snap, &index).unwrap_err();
    assert_eq!(
        err,
        AgentResolveError::AmbiguousSession {
            name: "builder".to_owned(),
            terminal: ResourceId::local(102),
            candidates: ids(&[902, 903]),
        }
    );
    assert_eq!(err.exit_code(), 2);
}
