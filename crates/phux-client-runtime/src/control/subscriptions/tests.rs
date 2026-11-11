use phux_protocol::ids::{ResourceId, ResourceKind, SessionId, WindowId};
use phux_protocol::wire::frame::{Command, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};

use super::{KindPolicy, Origin, STREAMED, Subscriptions};
use crate::control::{ControlOptions, ControlPlane, Topology};

fn terminal() -> ResourceId {
    ResourceId::local(1)
}

fn surface() -> ResourceId {
    ResourceId::local(3)
}

/// A plane whose topology lists a Terminal and a `PixelSurface` it draws.
fn plane_with_surface() -> ControlPlane {
    let session = SessionId::new(1);
    let snapshot = SessionSnapshot::new(session, WindowId::new(1), terminal())
        .with_sessions(vec![SessionInfo::new(session, "work")])
        .with_resources(vec![
            ResourceInfo::new(terminal(), WindowId::new(1), 80, 24).with_session(Some(session)),
            ResourceInfo::new(surface(), WindowId::new(0), 0, 0)
                .with_kind(ResourceKind::PixelSurface)
                .with_session(Some(session))
                .with_parent(Some(terminal())),
        ]);
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.topology = Some(Topology::from_snapshot(&snapshot));
    plane.handshake_ready = true;
    plane
}

fn frames(plane: &mut ControlPlane) -> Vec<FrameKind> {
    plane
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect()
}

fn resized(frames: &[FrameKind]) -> Vec<ResourceId> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::ResizeTerminal { terminal_id, .. } => Some(terminal_id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn only_a_kind_the_viewport_sizes_is_ever_resized() {
    let mut plane = plane_with_surface();
    assert_ne!(plane.attach_terminal(&surface()), 0);
    let attached = frames(&mut plane);
    assert!(attached.iter().any(|frame| matches!(frame,
        FrameKind::Command { command: Command::AttachResource { terminal_id, agent_after_seq: None, .. }, .. }
            if *terminal_id == surface())));
    assert!(resized(&attached).is_empty(), "a document has no geometry");

    assert_ne!(plane.attach_terminal(&terminal()), 0);
    assert_eq!(resized(&frames(&mut plane)), [terminal()]);

    plane.resize_viewport(100, 30);
    assert_eq!(
        resized(&frames(&mut plane)),
        [terminal()],
        "viewport fanout skips the document"
    );
    assert_eq!(
        plane.subscriptions.kind(&surface()),
        Some(ResourceKind::PixelSurface)
    );
}

#[test]
fn origins_overlap_and_an_entry_lives_while_any_remains() {
    let mut subscriptions = Subscriptions::default();
    let id = terminal();
    assert!(subscriptions.insert(&id, ResourceKind::Terminal, Origin::SessionAttach));
    assert!(subscriptions.insert(&id, ResourceKind::Terminal, Origin::AttachResource));
    assert!(!subscriptions.insert(&id, ResourceKind::Terminal, Origin::AttachResource));
    assert!(subscriptions.remove(&id, Origin::AttachResource));
    assert!(subscriptions.has_any(&id, STREAMED));
    subscriptions.clear(Origin::SessionAttach);
    assert!(!subscriptions.has_any(&id, STREAMED));
    assert_eq!(subscriptions.kind(&id), None, "an empty entry is pruned");

    subscriptions.set_ended(&id, true);
    assert!(subscriptions.ended(&id));
    subscriptions.clear_ended();
    assert!(!subscriptions.ended(&id));
    assert_eq!(subscriptions.kind(&id), None);
}

#[test]
fn per_kind_policy_is_one_table() {
    let terminal = KindPolicy::of(ResourceKind::Terminal);
    assert!(terminal.votes_on_geometry && terminal.joins_attach_barrier);
    assert!(!terminal.resumes_from_cursor);
    let agent = KindPolicy::of(ResourceKind::AgentSession);
    assert!(agent.resumes_from_cursor && !agent.votes_on_geometry && !agent.joins_attach_barrier);
    for kind in [
        ResourceKind::AccessibilityTree,
        ResourceKind::PixelSurface,
        ResourceKind::Document,
        ResourceKind::Unknown { tag: 90 },
    ] {
        assert_eq!(
            KindPolicy::of(kind),
            KindPolicy {
                votes_on_geometry: false,
                resumes_from_cursor: false,
                joins_attach_barrier: false,
            }
        );
    }
}
