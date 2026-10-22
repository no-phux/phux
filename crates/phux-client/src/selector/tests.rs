/// Every canonical form prints back as written, so a miss can name it.
#[test]
fn display_round_trips_through_parse() {
    for raw in [
        ".",
        "work",
        "work:2",
        "work:logs",
        "work:2.1",
        "work:logs.0",
        "@7",
        "build-box/@3",
        "#ci",
        "%claude",
    ] {
        let selector = super::parse(raw).expect(raw);
        assert_eq!(selector.to_string(), raw);
    }
}

use super::*;
use crate::layout::ArrangementDoc;
use phux_protocol::ids::{SessionId, WindowId};
use phux_protocol::wire::info::SessionSnapshot;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, WindowInfo};

/// The session-name rule (`rename::check_session_name`) is exactly "the
/// selector parses back to this session": names it accepts round-trip,
/// and every non-blank name it refuses parses as something else.
#[test]
fn session_name_rule_matches_the_selector_grammar() {
    let names = [
        "work",
        ".config",
        "a.b",
        "a/b",
        "a b",
        "x@y",
        "a=b",
        "w-2",
        ".",
        "=",
        "@3",
        "@x",
        "#tag",
        "#",
        "%agent",
        "%",
        "x:y",
        ":",
        "a:1.2",
        "devbox/@7",
        "/@1",
        "^abcdef",
        "^",
    ];
    for name in names {
        let addressable =
            matches!(parse(name), Ok(Selector::Session(ref parsed)) if parsed == name);
        assert_eq!(
            crate::rename::check_session_name(name).is_ok(),
            addressable,
            "{name:?}"
        );
    }
}

#[test]
fn an_explicit_id_is_taken_as_given() {
    assert_eq!(
        parse("@9").unwrap().explicit_id(),
        Some(ResourceId::local(9))
    );
    assert_eq!(
        parse("edge/@4").unwrap().explicit_id(),
        Some(ResourceId::satellite("edge", 4))
    );
    assert_eq!(parse("work").unwrap().explicit_id(), None);
}

#[test]
fn parse_accepts_every_form() {
    let session = |s: &str| s.to_owned();
    for (raw, expected) in [
        (".", Selector::Current),
        ("work", Selector::Session(session("work"))),
        (
            "work:1",
            Selector::Group(session("work"), GroupRef::Index(1)),
        ),
        (
            "work:editor",
            Selector::Group(session("work"), GroupRef::Tag(session("editor"))),
        ),
        (
            "work:1.2",
            Selector::Pane(session("work"), GroupRef::Index(1), 2),
        ),
        ("@42", Selector::ResourceId(42)),
        (
            "devbox/@42",
            Selector::SatelliteResourceId {
                host: session("devbox"),
                id: 42,
            },
        ),
        (
            "/@1",
            Selector::SatelliteResourceId {
                host: String::new(),
                id: 1,
            },
        ),
        ("#build", Selector::Tag(session("build"))),
        ("%build", Selector::Agent(session("build"))),
        ("%r2-d2_9", Selector::Agent(session("r2-d2_9"))),
        ("^AB23cd", Selector::Key(session("ab23cd"))),
    ] {
        assert_eq!(parse(raw).unwrap(), expected, "{raw}");
    }
}

#[test]
fn parse_rejects_malformed_selectors() {
    assert_eq!(parse(""), Err(ParseError::Empty));
    assert!(matches!(parse("@nope"), Err(ParseError::BadResourceId(_))));
    assert!(matches!(
        parse("devbox/@nope"),
        Err(ParseError::BadResourceId(_))
    ));
    assert!(matches!(
        parse("work:1.x"),
        Err(ParseError::BadPaneIndex(_))
    ));
    assert_eq!(parse("#"), Err(ParseError::EmptyTag));
    assert_eq!(parse("="), Err(ParseError::LastUnsupported));
    assert_eq!(parse("%"), Err(ParseError::EmptyAgentName));
    for bad in ["^", "^abc", "^ab1cde", &"a".repeat(30)] {
        let raw = if bad.starts_with('^') {
            bad.to_owned()
        } else {
            format!("^{bad}")
        };
        assert!(
            matches!(parse(&raw), Err(ParseError::BadKeyPrefix(_))),
            "{raw}"
        );
    }
    // Hub-local: `%a/@1` is a bad agent name, never a satellite id.
    let longest = "a".repeat(AGENT_NAME_MAX_LEN);
    assert_eq!(
        parse(&format!("%{longest}")).unwrap(),
        Selector::Agent(longest.clone())
    );
    for bad in [
        "%a/@1".to_owned(),
        "%Build".to_owned(),
        "%9lives".to_owned(),
        "%-lead".to_owned(),
        "%_lead".to_owned(),
        "%my agent".to_owned(),
        "%añejo".to_owned(),
        format!("%{longest}a"),
    ] {
        assert!(
            matches!(parse(&bad), Err(ParseError::BadAgentName(_))),
            "{bad}"
        );
    }
    assert!(is_addressable_agent_name("build"));
    assert!(!is_addressable_agent_name("Build Runner"));
    assert!(!is_addressable_agent_name(""));
}

/// Session "work" (id 1) with windows 0 (@100) and 1 (@101, @102), plus
/// session "play" (@200).
fn fixture() -> SessionSnapshot {
    let work = SessionId::new(1);
    let play = SessionId::new(2);
    let w0 = WindowId::new(10);
    let w1 = WindowId::new(11);
    let p0 = WindowId::new(20);
    SessionSnapshot::new(work, w0, ResourceId::local(100))
        .with_sessions(vec![
            SessionInfo::new(work, "work"),
            SessionInfo::new(play, "play"),
        ])
        .with_windows(vec![
            WindowInfo::new(w0, work, "shell").with_index(0),
            WindowInfo::new(w1, work, "editor").with_index(1),
            WindowInfo::new(p0, play, "shell").with_index(0),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(100), w0, 80, 24),
            ResourceInfo::new(ResourceId::local(101), w1, 80, 24),
            ResourceInfo::new(ResourceId::local(102), w1, 80, 24),
            ResourceInfo::new(ResourceId::local(200), p0, 80, 24),
        ])
}

fn ids(locals: &[u32]) -> Vec<ResourceId> {
    locals.iter().map(|id| ResourceId::local(*id)).collect()
}

#[test]
fn resolve_maps_each_form_against_the_snapshot() {
    let mut snap = fixture();
    snap.resources.push(ResourceInfo::new(
        ResourceId::satellite("devbox", 7),
        WindowId::new(999),
        120,
        40,
    ));
    for (raw, expected) in [
        (".", ids(&[100, 101, 102])),
        ("work", ids(&[100, 101, 102])),
        ("work:1", ids(&[101, 102])),
        ("work:editor", ids(&[101, 102])),
        ("work:1.1", ids(&[102])),
        ("@100", ids(&[100])),
        ("devbox/@7", vec![ResourceId::satellite("devbox", 7)]),
        // Direct ids still require inventory membership.
        ("other/@7", vec![]),
        ("devbox/@8", vec![]),
        ("@999", vec![]),
        ("ghost", vec![]),
    ] {
        assert_eq!(resolve(&parse(raw).unwrap(), &snap), expected, "{raw}");
    }
}

#[test]
fn resolve_tag_returns_every_tagged_terminal_in_snapshot_order() {
    let snap = fixture();
    let mut tags = TagIndex::new();
    tags.insert(
        ResourceId::local(100),
        vec!["build".to_owned(), "ci".to_owned()],
    );
    tags.insert(ResourceId::local(200), vec!["build".to_owned()]);
    tags.insert(ResourceId::local(101), vec!["web".to_owned()]);

    let build = resolve_with_tags(&parse("#build").unwrap(), &snap, &tags);
    assert_eq!(build, ids(&[100, 200]));
    assert!(resolve_with_tags(&parse("#nope").unwrap(), &snap, &tags).is_empty());
    assert!(resolve(&parse("#build").unwrap(), &snap).is_empty());
}

#[test]
fn terminal_id_formatter_emits_parseable_canonical_selectors() {
    for id in [
        ResourceId::local(7),
        ResourceId::satellite("devbox", 42),
        ResourceId::satellite("", 43),
        ResourceId::satellite("region/@rack/@node", 44),
        ResourceId::satellite("日本語 /@ host", 45),
        ResourceId::satellite("@prod", 46),
    ] {
        let rendered = format_terminal_id(&id);
        let mut snap = fixture();
        snap.resources
            .push(ResourceInfo::new(id.clone(), WindowId::new(999), 80, 24));
        assert_eq!(resolve(&parse(&rendered).unwrap(), &snap), vec![id]);
    }
}

#[test]
fn pick_target_pane_prefers_focused_then_first_then_none() {
    let [a, b, c] = [1, 2, 3].map(ResourceId::local);
    assert_eq!(
        pick_target_pane(&[a.clone(), b.clone()], &b),
        Some(b.clone())
    );
    assert_eq!(pick_target_pane(&[a.clone(), c], &b), Some(a));
    assert_eq!(pick_target_pane(&[], &b), None);
}

mod agent;

/// From a server that reports resource sessions, an unarranged session is
/// one group: its synthesized window is never read, so only index 0 and
/// the synthesized name resolve.
#[test]
fn an_unarranged_session_on_a_facet_server_is_one_group() {
    let work = SessionId::new(1);
    let synthesized = WindowId::new(10);
    let snap = SessionSnapshot::new(work, synthesized, ResourceId::local(100))
        .with_sessions(vec![SessionInfo::new(work, "work")])
        .with_windows(vec![WindowInfo::new(synthesized, work, "window-0")])
        .with_resources(
            [100, 101]
                .into_iter()
                .map(|id| {
                    ResourceInfo::new(ResourceId::local(id), WindowId::new(999), 80, 24)
                        .with_session(Some(work))
                })
                .collect(),
        );
    for (raw, expected) in [
        ("work:0", ids(&[100, 101])),
        ("work:0.1", ids(&[101])),
        ("work:1", vec![]),
        ("work:window-0", ids(&[100, 101])),
        ("work:shell", vec![]),
    ] {
        assert_eq!(resolve(&parse(raw).unwrap(), &snap), expected, "{raw}");
    }
}

/// `name:N` and `name:N.M` resolve against the arrangement when it is
/// known: group N by position or name, leaf M in depth-first order.
#[test]
fn group_forms_resolve_against_the_arrangement() {
    use crate::layout::{Group, LayoutNode, LayoutState, SplitDir};
    let snap = fixture();
    let leaf = |id| LayoutNode::Leaf(ResourceId::local(id));
    let arrangement = ArrangementDoc {
        groups: vec![
            Group::new(
                "shell".to_owned(),
                LayoutState::single(ResourceId::local(100)),
            ),
            Group::new(
                "editor".to_owned(),
                LayoutState {
                    tree: Some(LayoutNode::Split {
                        dir: SplitDir::Horizontal,
                        ratio: 0.5,
                        left: Box::new(leaf(102)),
                        right: Box::new(LayoutNode::Split {
                            dir: SplitDir::Vertical,
                            ratio: 0.5,
                            left: Box::new(leaf(101)),
                            right: Box::new(leaf(999)),
                        }),
                    }),
                    focus: None,
                },
            ),
        ],
        active: 0,
    };
    let arrangements = Arrangements::from([(SessionId::new(1), arrangement)]);
    let context = ResolveContext {
        arrangements: Some(&arrangements),
        ..ResolveContext::default()
    };
    for (raw, expected) in [
        ("work:1", ids(&[102, 101])),
        ("work:editor", ids(&[102, 101])),
        ("work:1.0", ids(&[102])),
        ("work:1.1", ids(&[101])),
        ("work:0.0", ids(&[100])),
        ("work:2", vec![]),
        // A session without a known arrangement keeps its legacy windows.
        ("play:0", ids(&[200])),
    ] {
        assert_eq!(
            resolve_in(&parse(raw).unwrap(), &snap, &context),
            expected,
            "{raw}"
        );
    }
}

/// `^key` names a session's or a resource's record, through the
/// organization records; without them it misses.
#[test]
fn key_prefixes_resolve_through_the_organization_records() {
    use crate::testkit::OrgFixture;
    use phux_protocol::wire::frame::SessionLifetime;
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let shell = fixture.shell(work);
    let other = fixture.shell(work);
    let snap = fixture.snapshot();
    let inventory = crate::org::OrgInventory {
        authority: phux_protocol::ids::AuthorityId::new([1; 16]).unwrap(),
        inbox: None,
        sessions: fixture.sessions().to_vec(),
        resources: fixture.resources().to_vec(),
        snapshot_revision: phux_protocol::ids::Revision::new(1),
        journal_seq: 0,
        total_intake: 0,
        degradation: crate::state::Degradation::default(),
    };
    let context = ResolveContext {
        organization: Some(&inventory),
        ..ResolveContext::default()
    };
    let key = |text: String| parse(&format!("^{}", &text[..8])).unwrap();
    assert_eq!(
        resolve_in(&key(shell.to_string()), &snap, &context),
        vec![fixture.live(&shell).unwrap()]
    );
    assert_eq!(
        resolve_in(&key(work.to_string()), &snap, &context),
        vec![fixture.live(&shell).unwrap(), fixture.live(&other).unwrap()]
    );
    assert!(resolve(&key(work.to_string()), &snap).is_empty());
}
