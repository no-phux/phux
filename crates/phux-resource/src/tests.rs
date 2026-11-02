use super::*;

#[derive(Debug)]
struct StubFacet(u16);

impl ResourceFacet for StubFacet {
    const KIND: ResourceKind = ResourceKind::Terminal;
}

impl ResourceOps for StubFacet {
    fn resize(&self, _: (u16, u16), _: Option<(u16, u16)>) -> bool {
        false
    }

    fn upgrade_sender(&self) -> Option<mpsc::Sender<handoff::UpgradeHandleRequest>> {
        None
    }

    fn lease_changed(&self, _: LeaseChange) -> Delivery {
        Box::pin(std::future::ready(()))
    }

    fn try_lease_changed(&self, _: LeaseChange) {}

    fn retire(&self) -> Delivery {
        Box::pin(std::future::ready(()))
    }
}

#[derive(Debug)]
struct OtherFacet;

impl ResourceFacet for OtherFacet {
    const KIND: ResourceKind = ResourceKind::AgentSession;
}

impl ResourceOps for OtherFacet {
    fn resize(&self, _: (u16, u16), _: Option<(u16, u16)>) -> bool {
        false
    }

    fn upgrade_sender(&self) -> Option<mpsc::Sender<handoff::UpgradeHandleRequest>> {
        None
    }

    fn lease_changed(&self, _: LeaseChange) -> Delivery {
        Box::pin(std::future::ready(()))
    }

    fn try_lease_changed(&self, _: LeaseChange) {}

    fn retire(&self) -> Delivery {
        Box::pin(std::future::ready(()))
    }
}

fn stub_handle() -> ResourceHandle {
    let (core, channels) = ResourceCore::new(
        ResourceKind::Terminal,
        None,
        CancellationToken::new(),
        DEFAULT_OUTPUT_BROADCAST,
    );
    drop(core);
    ResourceHandle::new(None, channels.output, StubFacet(80))
}

#[test]
fn a_facet_resolves_only_for_its_own_kind() {
    let handle = stub_handle();
    assert_eq!(handle.kind, ResourceKind::Terminal);
    assert_eq!(handle.facet::<StubFacet>().expect("own facet").0, 80);
    assert_eq!(
        handle
            .facet::<OtherFacet>()
            .expect_err("another kind's facet"),
        WrongResourceKind {
            required: ResourceKind::AgentSession,
            actual: ResourceKind::Terminal,
        }
    );
}

#[test]
fn a_companion_resolves_by_type_and_the_newest_wins() {
    let handle = stub_handle();
    assert_eq!(handle.companion::<u8>(), None);
    let handle = handle.with_companion(1_u8).with_companion("other");
    assert_eq!(handle.companion::<u8>(), Some(&1));
    assert_eq!(
        handle.clone().with_companion(2_u8).companion::<u8>(),
        Some(&2)
    );
    assert_eq!(handle.companion::<&str>(), Some(&"other"));
}

#[test]
fn next_seq_is_checked() {
    let (mut core, _channels) = ResourceCore::new(
        ResourceKind::Terminal,
        None,
        CancellationToken::new(),
        DEFAULT_OUTPUT_BROADCAST,
    );
    assert_eq!(core.next_seq(), Some(1));
    assert_eq!(core.seq(), 1);
    core.seq = u64::MAX;
    assert_eq!(core.next_seq(), None, "an exhausted sequence never wraps");
    assert_eq!(core.seq(), u64::MAX);
}

#[test]
fn exit_notify_fires_once() {
    let (mut core, channels) = ResourceCore::new(
        ResourceKind::Terminal,
        None,
        CancellationToken::new(),
        DEFAULT_OUTPUT_BROADCAST,
    );
    core.notify_exit(phux_core::process::ExitOutcome::exited(3));
    core.notify_exit(phux_core::process::ExitOutcome::signaled(9));
    assert_eq!(
        channels.exit_notify.blocking_recv(),
        Ok(phux_core::process::ExitOutcome::exited(3))
    );
}
