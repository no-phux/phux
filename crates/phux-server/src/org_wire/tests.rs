use super::*;

/// ADR-0168: the `project.*` attribute pairs become one typed identity
/// on the record and come back out as the same pairs; unknown names drop.
#[test]
fn repository_attrs_round_trip_through_the_typed_record() {
    let identity = phux_core::project_identity::SessionProject {
        id: "github.com/no-phux/phux".to_owned(),
        name: "phux".to_owned(),
        checkout: "/w/phux-feat".to_owned(),
        branch: Some("feat".to_owned()),
        linked_worktree: true,
        host: Some("mini".to_owned()),
    };
    let mut pairs = vec![("project".to_owned(), "label".to_owned())];
    pairs.extend(identity.to_attrs());
    pairs.push(("mission.task".to_owned(), "abc".to_owned()));
    pairs.push(("unknown".to_owned(), "x".to_owned()));
    let core = core_attrs(&pairs);
    assert_eq!(core.repository.as_deref(), Some(&identity));
    assert_eq!(core.project.as_deref(), Some("label"));
    assert_eq!(core.mission_task.as_deref(), Some("abc"));
    let back = attrs(&core);
    assert!(!back.iter().any(|(name, _)| name == "unknown"));
    assert!(
        back.iter()
            .any(|(name, value)| name == "mission.task" && value == "abc")
    );
    assert_eq!(core_attrs(&back), core);
}

#[test]
fn keys_and_refusals_round_trip() {
    let core = phux_core::SessionKey::from_bytes([3; 16]);
    let wire = session_key(core).expect("non-zero");
    assert_eq!(core_session_key(&wire), core);
    assert_eq!(
        refusal(&phux_core::Refusal::InvalidArgument {
            field: "name",
            reason: "empty".to_owned()
        }),
        frame::Refusal::InvalidArgument {
            field: "name".to_owned(),
            reason: "empty".to_owned()
        }
    );
    assert_eq!(
        outcome(phux_core::Outcome::Exit(phux_core::ExitOutcome::signaled(
            9
        ))),
        frame::Outcome::Signaled(9)
    );
}

#[test]
fn documents_round_trip() {
    let groups = vec![phux_core::Group {
        id: [1; 16],
        name: Some("main".to_owned()),
        tree: phux_core::LayoutNode::Split {
            dir: phux_core::SplitDir::Vertical,
            ratio: 0.5,
            left: Box::new(phux_core::LayoutNode::Leaf(phux_core::EntryRef::member(
                [2; 16],
                phux_core::ResourceKey::from_bytes([4; 16]),
            ))),
            right: Box::new(phux_core::LayoutNode::Leaf(phux_core::EntryRef {
                entry: [3; 16],
                target: phux_core::EntryTarget::Satellite {
                    host: "far".to_owned(),
                    id: 9,
                },
            })),
        },
    }];
    assert_eq!(core_groups(&document(&groups)), Some(groups));
}
