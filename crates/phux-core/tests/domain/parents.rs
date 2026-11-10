//! The organization's parent rule: structure only. A kind that binds to a
//! parent needs one that exists and can supervise a producer; whether a
//! live parent is fit is its app's check at spawn.

#![allow(clippy::expect_used, clippy::panic)]

use phux_core::resource::{DocumentLaunch, SurfaceLaunch, TreeLaunch};
use phux_core::{
    AgentLaunch, LaunchSpec, NewResource, Op, Refusal, ResourceKey, ResourceKind, SessionLifetime,
};

use crate::support::{Harness, created, shell};

/// A launch for each kind that binds to a parent.
fn projections() -> [LaunchSpec; 4] {
    [
        LaunchSpec::Agent(AgentLaunch {
            provider: "claude".to_owned(),
            native_id: None,
        }),
        LaunchSpec::Tree(TreeLaunch {
            target: "Finder".to_owned(),
        }),
        LaunchSpec::Surface(SurfaceLaunch {
            target: "display".to_owned(),
        }),
        LaunchSpec::Document(DocumentLaunch {
            content_type: "text/markdown".to_owned(),
        }),
    ]
}

fn under(parent: Option<ResourceKey>, launch: LaunchSpec) -> NewResource {
    NewResource {
        kind: launch.kind(),
        launch: Some(launch),
        parent,
        ..shell()
    }
}

fn create(h: &mut Harness, spec: NewResource) -> Result<ResourceKey, Refusal> {
    h.run(Op::ResourceCreate {
        placement: None,
        session: None,
        resource: spec,
    })
    .map(|applied| created(&applied).1.expect("a resource"))
}

fn parent_refusal(result: Result<ResourceKey, Refusal>) -> String {
    match result {
        Err(Refusal::InvalidArgument {
            field: "parent",
            reason,
        }) => reason,
        other => panic!("a parent refusal, got {other:?}"),
    }
}

#[test]
fn every_kind_answers_both_parent_questions() {
    for kind in [
        ResourceKind::Terminal,
        ResourceKind::AgentSession,
        ResourceKind::AccessibilityTree,
        ResourceKind::PixelSurface,
        ResourceKind::Document,
    ] {
        assert_ne!(
            kind.requires_parent(),
            kind.can_parent(),
            "{kind}: a projection binds to a parent; a parent runs a process"
        );
    }
}

#[test]
fn a_projection_binds_under_a_resource_that_can_supervise_its_producer() {
    let mut h = Harness::new();
    let s = h.create_session("work", SessionLifetime::Persistent);
    let terminal = h.shell_in(s);
    for launch in projections() {
        let kind = launch.kind();
        let reason = parent_refusal(create(&mut h, under(None, launch.clone())));
        assert!(reason.contains("needs a parent"), "{kind}: {reason}");

        let child = create(&mut h, under(Some(terminal), launch.clone()))
            .unwrap_or_else(|refusal| panic!("{kind} under a terminal: {refusal:?}"));
        assert_eq!(h.resource(child).provenance.parent, Some(terminal));

        // A projection runs no process, so nothing binds under one.
        for nested in projections() {
            let reason = parent_refusal(create(&mut h, under(Some(child), nested)));
            assert!(reason.contains("cannot supervise"), "{kind}: {reason}");
        }
    }
}

#[test]
fn a_parent_must_exist() {
    let mut h = Harness::new();
    let missing = ResourceKey::from_bytes([0x5a; 16]);
    for launch in projections() {
        let reason = parent_refusal(create(&mut h, under(Some(missing), launch)));
        assert_eq!(reason, "unknown resource");
    }
}
