//! Real control-dial approval review, not an overlay-only fixture.
use super::*;

#[test]
fn real_credential_requests_can_be_reviewed_approved_denied_and_stale() {
    use phux_client::credentials;
    use phux_server_testkit::{
        SOCKET_CONNECT_DEADLINE, run_local, spawn_server, wait_for_raw_socket,
    };

    run_local(async {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("approval.sock");
        let (shutdown, server) = spawn_server(socket.clone(), None);
        drop(wait_for_raw_socket(&socket, SOCKET_CONNECT_DEADLINE).await);
        let mut owner = Connection::connect(&socket).await.unwrap();
        let mut provider = Connection::connect(&socket).await.unwrap();
        assert!(
            credentials::next(&mut provider, "desktop")
                .await
                .unwrap()
                .is_none()
        );
        for key in [PhysicalKey::A, PhysicalKey::D] {
            credential_case(&socket, &mut owner, &mut provider, key).await;
        }
        drop(provider);
        drop(owner);
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}

#[allow(
    clippy::future_not_send,
    reason = "TUI overlays are current-thread bound"
)]
async fn credential_case(
    socket: &std::path::Path,
    owner: &mut Connection,
    provider: &mut Connection,
    key: PhysicalKey,
) {
    use phux_client::credentials::{self, CredentialRequest};

    let mut requester = Connection::connect(socket).await.unwrap();
    let request = CredentialRequest {
        provider: "desktop".to_owned(),
        references: vec!["op://vault/item/field".to_owned()],
        purpose: "native TUI approval test".to_owned(),
    };
    let queued_request = request.clone();
    let waiting =
        tokio::spawn(async move { credentials::request(&mut requester, queued_request).await });
    let pending = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pending = phux_client::approvals::list(owner).await.unwrap();
            if !pending.is_empty() {
                break pending;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(pending.len(), 1);
    let mut list = picker(&pending, &Theme::default());
    let OverlayCommand::Commit(selected) = list.handle_key(&press(PhysicalKey::Enter)) else {
        panic!("picker must open a review, never decide")
    };
    assert_eq!(selected.action, "approval-review");
    let mut review = manage(owner, &selected, &Theme::default()).await.unwrap();
    let area = Rect::new(0, 0, 160, 50);
    let mut buf = Buffer::empty(area);
    review.render(area, &mut buf);
    let text: String = buf
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(text.contains("op://vault/item/field") && text.contains("Bound provider: client"));
    let OverlayCommand::Commit(decision) = review.handle_key(&press(key)) else {
        panic!("decision")
    };
    let result = manage(owner, &decision, &Theme::default()).await.unwrap();
    result.render(area, &mut buf);
    let text: String = buf
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(text.contains("Server accepted"), "{text}");
    let stale = manage(owner, &decision, &Theme::default()).await;
    assert!(stale.err().unwrap().contains("Stale approval"));

    if key == PhysicalKey::A {
        // Empty claims are confirmed non-deliveries. Wait for readiness without
        // retrying any claim whose outcome is uncertain.
        let job = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(job) = credentials::next(provider, "desktop").await.unwrap() {
                    break job;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(job.request, request);
        // No vault or resolved value is needed to test the approval UI.
        credentials::complete(provider, job.id, None).await.unwrap();
    } else {
        assert!(
            credentials::next(provider, "desktop")
                .await
                .unwrap()
                .is_none(),
            "denied requests never reach the provider"
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(
        phux_client::approvals::list(owner)
            .await
            .unwrap()
            .is_empty()
    );
}
