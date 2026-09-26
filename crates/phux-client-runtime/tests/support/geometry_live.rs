use super::*;
use phux_client_runtime::control::TerminalResizeOutcome as Resize;
use phux_protocol::wire::frame::RolePolicy;

fn resource_options(role: Option<RolePolicy>) -> ClientOptions {
    let mut options = options();
    options.control.attach = None;
    options.control.attach_role = role;
    options.control.viewport = (5, 2);
    options.control.auto_attach_foreign_spawns = false;
    options
}

async fn subscribed(client: &Client, terminal: &ResourceId) {
    wait_for_event(client, "resource subscription", |event| match event {
        Event::TerminalAttached {
            terminal_id, error, ..
        } if terminal_id == terminal => {
            assert!(error.is_none(), "{error:?}");
            Some(())
        }
        _ => None,
    })
    .await;
    wait_until("resource bootstrap", || client.has_projection(terminal)).await;
}

async fn geometry(client: &Client, terminal: &ResourceId, size: (u16, u16)) {
    wait_until("authoritative geometry", || {
        client
            .acquire(terminal)
            .is_some_and(|frame| (frame.cols, frame.rows) == size)
    })
    .await;
}

#[test]
fn targeted_geometry_and_observer_reconnect_use_authoritative_server_frames() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("geometry.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let owner = Runtime::connect(Target::uds(&socket), options()).unwrap();
        wait_for_status(&owner, Status::Attached).await;
        let first = spawn_cat(&owner).await;
        let second = spawn_cat(&owner).await;

        let desktop = Runtime::connect(Target::uds(&socket), resource_options(None)).unwrap();
        wait_for_status(&desktop, Status::Attached).await;
        assert_ne!(desktop.attach_terminal_preserving_geometry(&first), 0);
        subscribed(&desktop, &first).await;
        assert_ne!(desktop.attach_terminal_preserving_geometry(&second), 0);
        subscribed(&desktop, &second).await;
        geometry(&desktop, &first, (40, 8)).await;
        geometry(&desktop, &second, (40, 8)).await;
        assert_eq!(desktop.resize_terminal(&first, 55, 9), Resize::Queued);
        geometry(&desktop, &first, (55, 9)).await;
        geometry(&owner, &first, (55, 9)).await;
        geometry(&desktop, &second, (40, 8)).await;
        geometry(&owner, &second, (40, 8)).await;

        let observer = Runtime::connect(
            Target::uds(&socket),
            resource_options(Some(RolePolicy::VIEWER)),
        )
        .unwrap();
        wait_for_status(&observer, Status::Attached).await;
        assert_ne!(observer.attach_terminal_preserving_geometry(&first), 0);
        subscribed(&observer, &first).await;
        geometry(&observer, &first, (55, 9)).await;
        assert_eq!(observer.resize_terminal(&first, 5, 2), Resize::Observer);
        let _ = observer.take_events();
        observer.resync();
        subscribed(&observer, &first).await;
        geometry(&observer, &first, (55, 9)).await;
        geometry(&owner, &first, (55, 9)).await;
        geometry(&owner, &second, (40, 8)).await;
        assert_eq!(observer.with_control(|plane| plane.viewport()), (5, 2));
        observer.close();
        desktop.close();
        owner.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}
