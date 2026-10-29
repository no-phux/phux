use super::*;
use crate::hub::relay::{HubRelays, RelayHandle, RelayMailbox, RelayRequest};
use phux_protocol::ids::{ResourceId, SatelliteHost};

struct Fixture {
    state: SharedState,
    host: SatelliteHost,
    client_id: ClientId,
    handle: RelayHandle,
    mailbox: RelayMailbox,
    out_tx: tokio::sync::mpsc::Sender<Outbound>,
}

impl Fixture {
    fn new() -> Self {
        let state = SharedState::new();
        let host = SatelliteHost::from("sat");
        let (handle, mailbox) = RelayHandle::new(host.clone());
        let relays = HubRelays::default();
        relays.insert(handle.clone());
        let client_id = ClientId(1);
        let token = CancellationToken::new();
        state.with_mut(|s| {
            s.set_hub_relays(relays);
            s.set_client_connection_cancellation(client_id, token);
        });
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel(8);
        Self {
            state,
            host,
            client_id,
            handle,
            mailbox,
            out_tx,
        }
    }

    fn wire(&self, terminal: u32) -> ResourceId {
        ResourceId::satellite(self.host.clone(), terminal)
    }

    async fn refuse_attach(&mut self, terminal: u32, role: RolePolicy) -> CommandResult {
        self.settle_attach(
            terminal,
            role,
            CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: "no such terminal".to_owned(),
            },
        )
        .await
    }

    async fn settle_attach(
        &mut self,
        terminal: u32,
        role: RolePolicy,
        result: CommandResult,
    ) -> CommandResult {
        let local = ResourceId::local(terminal);
        let command = Command::attach_terminal(local.clone(), Some(role));
        let target = SatelliteLeaseTarget::new(&self.state, &self.host, self.client_id, &local);
        let ctx = CommandContext::for_test(&self.state, self.client_id, &self.out_tx);
        let attach = relay_satellite_attach(&ctx, &target, &self.handle, &command, role);
        tokio::pin!(attach);
        assert!(
            futures_util::poll!(&mut attach).is_pending(),
            "attach must wait on the satellite reply"
        );
        let RelayRequest::Command { reply, .. } = self
            .mailbox
            .requests
            .try_recv()
            .expect("attach must reach the link")
        else {
            panic!("expected a relayed ATTACH_RESOURCE");
        };
        reply.send(result).expect("attach is waiting");
        attach.await
    }
}

/// A refused first VIEWER attach must not leave a hub-side tombstone on
/// a satellite id the hub never subscribed.
#[tokio::test]
async fn a_refused_first_satellite_viewer_attach_does_not_leave_a_tombstone() {
    let mut fixture = Fixture::new();
    let result = fixture.refuse_attach(1, RolePolicy::VIEWER).await;
    assert!(matches!(
        result,
        CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            ..
        }
    ));
    let wire = fixture.wire(1);
    assert!(
        !fixture
            .state
            .with(|s| s.is_viewer(fixture.client_id, &wire))
    );
    assert!(fixture.state.with(|s| s.terminal_viewers(&wire).is_empty()));
}

/// A refused narrowing keeps a prior mark and a proxy-attached mark.
#[tokio::test]
async fn a_refused_satellite_viewer_attach_keeps_a_prior_mark_or_proxy() {
    let mut fixture = Fixture::new();
    let prior = fixture.wire(3);
    fixture.state.with_mut(|s| {
        s.set_viewer_mark(fixture.client_id, &prior, true);
    });
    let _ = fixture.refuse_attach(3, RolePolicy::VIEWER).await;
    assert!(
        fixture
            .state
            .with(|s| s.is_viewer(fixture.client_id, &prior)),
        "a refused re-attach must not shed a prior viewer tombstone"
    );

    let attached = fixture.wire(9);
    fixture.state.with_mut(|s| {
        s.register_satellite_proxy_attach(fixture.client_id, fixture.host.clone(), 9);
    });
    let _ = fixture.refuse_attach(9, RolePolicy::VIEWER).await;
    assert!(
        fixture
            .state
            .with(|s| s.is_viewer(fixture.client_id, &attached)),
        "a refused narrowing of an existing proxy attach keeps the mark"
    );
}

#[tokio::test]
async fn a_successful_satellite_viewer_attach_keeps_the_mark() {
    let mut fixture = Fixture::new();
    let result = fixture
        .settle_attach(7, RolePolicy::VIEWER, CommandResult::Ok)
        .await;
    assert!(matches!(result, CommandResult::Ok));
    let wire = fixture.wire(7);
    assert!(
        fixture
            .state
            .with(|s| s.is_viewer(fixture.client_id, &wire))
    );
    assert!(fixture.state.with(|s| s.has_satellite_proxy_attach(
        fixture.client_id,
        &fixture.host,
        7
    )));
}
