//! Controlled engines for explicit agent requests and aggregate document cuts.
use super::*;

pub(super) fn agent_stub(
    state: &SharedState,
    parent: phux_core::ids::ResourceId,
    session: phux_core::ids::SessionId,
) -> (
    phux_core::ids::ResourceId,
    mpsc::Receiver<phux_agent_session::session::BootstrapRequest>,
) {
    let core = state.with_mut(|s| {
        let core = s
            .new_agent_session(
                parent,
                &phux_core::resource::AgentFacet {
                    provider: "fixture".to_owned(),
                    native_id: None,
                    state: None,
                },
            )
            .unwrap();
        s.move_resource_to(core, session, &Actor::owner()).unwrap();
        core
    });
    let (bootstrap, rx) = mpsc::channel(4);
    let (append, _append_rx) = mpsc::channel(1);
    let (output, _) = tokio::sync::broadcast::channel(8);
    let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = ResourceHandle::new(
        Some(parent),
        output,
        phux_agent_session::session::AgentSessionHandle {
            provider: "fixture".into(),
            native_id: None,
            append,
            bootstrap,
            ended: ended.clone(),
        },
    )
    .with_companion(crate::resource::companions::LiveStream::new(ended));
    state.with_mut(|s| {
        s.register_resource_handle(core, handle, CancellationToken::new())
            .unwrap();
    });
    (core, rx)
}

pub(super) fn agent_cut(records: Vec<Bytes>) -> phux_agent_session::session::AgentSessionBootstrap {
    phux_agent_session::session::AgentSessionBootstrap {
        base_seq: 0,
        resume_after_seq: None,
        records,
        dropped: 0,
        ended: false,
        native_publications: Vec::new(),
        native_publication_high_water: 0,
    }
}

pub(super) fn parked_tree(
    state: &SharedState,
    parent: phux_core::ids::ResourceId,
    session: phux_core::ids::SessionId,
) -> (
    phux_core::ids::ResourceId,
    ResourceHandle,
    DocumentActor<TreeDocument>,
) {
    let core = state.with_mut(|s| {
        let core = s
            .create_document(
                parent,
                LaunchSpec::Tree(phux_core::resource::TreeLaunch {
                    target: "parked".to_owned(),
                }),
                &Actor::owner(),
            )
            .unwrap();
        s.move_resource_to(core, session, &Actor::owner()).unwrap();
        core
    });
    let token = CancellationToken::new();
    let bundle = DocumentActor::<TreeDocument>::build(parent, "parked", token.clone());
    state.with_mut(|s| {
        s.register_resource_handle(core, bundle.handle.clone(), token)
            .unwrap();
    });
    (core, bundle.handle, bundle.actor)
}

pub(super) async fn wait_for_capture(handle: &ResourceHandle) {
    let snapshot = &handle.document::<TreeDocument>().unwrap().snapshot;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while snapshot.capacity() == snapshot.max_capacity() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("aggregate capture queued the document snapshot");
}
