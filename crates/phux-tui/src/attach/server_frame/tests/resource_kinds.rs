//! Run document streams through the actual TUI handler and native VT adapter.

use super::*;

/// Shared graph fixture: one Terminal and its grid-free agent child.
pub(super) fn mixed_kind_snapshot(pane: &ResourceId, agent: &ResourceId) -> SessionSnapshot {
    let (window, session) = (WindowId::new(1), SessionId::new(1));
    SessionSnapshot::new(session, window, pane.clone())
        .with_sessions(vec![SessionInfo::new(session, "work".to_owned())])
        .with_windows(vec![WindowInfo::new(window, session, "w0".to_owned())])
        .with_resources(vec![
            ResourceInfo::new(pane.clone(), window, 100, 30),
            ResourceInfo::new(agent.clone(), WindowId::new(0), 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(pane.clone()))
                .with_agent(Some(
                    AgentFacet::new("claude", "working").with_native_id(Some("s-1".to_owned())),
                )),
        ])
}

fn document(h: &mut H, id: &ResourceId, profile: phux_protocol::BootstrapStreamProfile) {
    h.send(FrameKind::BootstrapBegin {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        profile,
        cols: 0,
        rows: 0,
        base_seq: 1,
    });
    h.send(chunk_frame(
        id,
        b"\x1b]52;c;bm90LXR0eQ==\x07untrusted document",
    ));
    h.send(ready_frame(id));
    h.send(output_frame(id, 2, b"\x1b[2Juntrusted live document"));
}

#[test]
fn attached_requests_only_agent_children_of_admitted_participants() {
    let mut snapshot = mixed_kind_snapshot(&tid(1), &tid(2));
    let mut orphan = snapshot.resources[1].clone();
    orphan.id = tid(3);
    orphan.parent = Some(tid(99));
    snapshot.resources.push(orphan.clone());
    orphan.id = tid(4);
    orphan.parent = None;
    snapshot.resources.push(orphan);
    let mut h = H::new();
    let outcome = h.send(attached(snapshot, 1));
    assert_eq!(outcome.attach_panes, vec![tid(2)]);
    assert!(h.mirror.engine_kernel.agent_session(&tid(3)).is_none());
    assert!(h.mirror.engine_kernel.agent_session(&tid(4)).is_none());
}

#[test]
fn mixed_document_ready_never_requires_a_terminal_replica_or_writes_document_bytes_to_tty() {
    let (pane, agent, tree, surface) = (tid(1), tid(2), tid(3), tid(4));
    let mut snapshot = mixed_kind_snapshot(&pane, &agent);
    snapshot.resources.extend([
        ResourceInfo::resource(tree.clone(), ResourceKind::AccessibilityTree)
            .with_session(Some(SessionId::new(1))),
        ResourceInfo::resource(surface.clone(), ResourceKind::PixelSurface)
            .with_session(Some(SessionId::new(1))),
    ]);
    // The server's default resource can be a document. The TUI must keep
    // keyboard/focus/layout ownership on the session's Terminal instead.
    snapshot.focused_resource = tree.clone();
    let mut h = H::new();
    h.send(attached(snapshot, 1));
    h.send(begin_frame(&pane));
    h.send(chunk_frame(&pane, b"terminal is usable"));
    h.send(ready_frame(&pane));
    let tty = h.out.clone();
    document(
        &mut h,
        &tree,
        phux_protocol::BootstrapStreamProfile::AccessibilityTreeJsonV1,
    );
    document(
        &mut h,
        &surface,
        phux_protocol::BootstrapStreamProfile::PixelSurfacePngV1,
    );
    assert_eq!(h.out, tty, "document content never reaches the TTY sink");
    for resource in [&tree, &surface] {
        let outcome = h.send(closed(resource, None));
        assert!(!outcome.exit && !outcome.reflow_panes && !outcome.emit_set_metadata);
    }
    h.send(FrameKind::AttachReady { attach_id: 1 });
    assert_eq!(h.mirror.focused_resource, Some(pane.clone()));
    assert_eq!(h.leaves().as_slice(), std::slice::from_ref(&pane));
    assert_eq!(h.mirror.panes.len(), 1);
    h.output(&pane, b"still live");
    assert!(h.mirror.engine_kernel.published(&pane).is_some());
}

#[test]
fn document_only_session_is_read_only_without_a_fake_terminal_slot() {
    let tree = tid(3);
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(0), tree.clone())
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "docs")])
        .with_resources(vec![
            ResourceInfo::resource(tree.clone(), ResourceKind::AccessibilityTree)
                .with_session(Some(SessionId::new(1))),
        ]);
    let mut h = H::new();
    h.send(attached(snapshot, 1));
    document(
        &mut h,
        &tree,
        phux_protocol::BootstrapStreamProfile::AccessibilityTreeJsonV1,
    );
    h.send(FrameKind::AttachReady { attach_id: 1 });
    assert!(h.mirror.focused_resource.is_none());
    assert!(h.mirror.panes.is_empty());
    assert!(h.leaves().is_empty());
    assert!(h.out.is_empty());
}
