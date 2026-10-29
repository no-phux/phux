//! Public MCP calls exercise the production server connection loop and real PTYs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use phux_protocol::{
    ids::{OperationId, ResourceId},
    wire::frame::{Command, CommandResult, CommandValue, OrgFilter, SessionLifetime},
};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, command, join_after_shutdown, run_local,
    spawn_server_seed_pty_no_cmd, wait_for_server_screen_text, wait_for_socket,
};
use serde_json::{Value, json};

async fn invoke(name: &str, socket: &std::path::Path, mut args: Value) -> Value {
    args["socket"] = json!(socket);
    let result = crate::tools::dispatch(name, &args).await.unwrap();
    crate::contract_tests::validate_output(name, &result);
    result
}
async fn count(stream: &mut tokio::net::UnixStream, session: &str) -> usize {
    let result = command(
        stream,
        91,
        Command::GetOrganization {
            filter: OrgFilter::Session(session.parse().unwrap()),
            page_size: 100,
            cursor: None,
        },
    )
    .await;
    let CommandResult::OkWith(CommandValue::OrganizationPage(page)) = result else {
        panic!("organization")
    };
    page.resources.len()
}
#[test]
fn public_create_places_executes_replays_and_preserves_failed_launches() {
    run_local(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("public-api.sock");
        let (shutdown, server) = spawn_server_seed_pty_no_cmd(socket.clone(), None);
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let result = command(
            &mut stream,
            1,
            Command::SessionCreate {
                operation: OperationId::new([1; 16]).unwrap(),
                name: "api".into(),
                lifetime: SessionLifetime::Persistent,
                attrs: vec![],
                launch: None,
            },
        )
        .await;
        let CommandResult::OkWith(CommandValue::Committed(receipt)) = result else {
            panic!("session")
        };
        let session = receipt.session.unwrap().to_string();
        let description = invoke("phux_api_describe", &socket, json!({})).await;
        assert_eq!(description["source"], "connected_server");
        assert_eq!(description["operations"][0]["name"], "terminal.create");
        let input =
            jsonschema::validator_for(&description["operations"][0]["input_schema"]).unwrap();
        let output =
            jsonschema::validator_for(&description["operations"][0]["output_schema"]).unwrap();
        let request = json!({"idempotency_key":"22222222222222222222222222222222","session":session,"argv":["/bin/sh","-c","printf 'API_READY\\n'; exec /bin/sleep 30"]});
        assert!(input.is_valid(&request));
        let first = invoke("phux_terminal_create", &socket, request.clone()).await;
        assert!(output.is_valid(&first));
        assert_eq!(first["execution"]["state"], "running");
        assert_eq!(first["authority"], description["authority"]);
        let id = ResourceId::local(u32::try_from(first["terminal_id"].as_u64().unwrap()).unwrap());
        wait_for_server_screen_text(&mut stream, &id, "API_READY", WIRE_RECV_TIMEOUT).await;
        let beside = json!({"idempotency_key":"33333333333333333333333333333333","session":session,"argv":["/bin/sleep","30"],"placement":{"type":"beside","entry":first["placement"]["entry"],"resource":first["resource"],"instance":first["execution"]["instance"],"activation":first["execution"]["activation"],"axis":"vertical","ratio":0.3}});
        assert!(input.is_valid(&beside));
        let second = invoke("phux_terminal_create", &socket, beside.clone()).await;
        assert_eq!(
            second["placement"]["arrangement"],
            first["placement"]["arrangement"]
        );
        assert_ne!(second["placement"]["entry"], first["placement"]["entry"]);
        assert_legacy_layout(
            &socket,
            &mut stream,
            &session,
            &[
                id.clone(),
                ResourceId::local(u32::try_from(second["terminal_id"].as_u64().unwrap()).unwrap()),
            ],
        )
        .await;
        let replay = invoke("phux_terminal_create", &socket, request.clone()).await;
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["resource"], first["resource"]);
        assert_eq!(replay["placement"], first["placement"]);
        assert_eq!(count(&mut stream, &session).await, 2);
        let recovered = invoke(
            "phux_terminal_operation",
            &socket,
            json!({"session":session,"idempotency_key":request["idempotency_key"]}),
        )
        .await;
        assert_eq!(recovered["resource"], first["resource"]);
        let dispatcher = crate::operations::dispatcher();
        let absent = dispatcher("phux_terminal_operation".into(), json!({"socket":socket,"session":session,"idempotency_key":"ffffffffffffffffffffffffffffffff"})).await.unwrap_err();
        assert_eq!(absent.document()["error"]["outcome"], "unknown");
        assert_eq!(absent.document()["error"]["code"], "receipt_not_retained");
        let mut missing = beside;
        missing["idempotency_key"] = json!("44444444444444444444444444444444");
        missing["placement"]["entry"] = json!("ffffffffffffffffffffffffffffffff");
        missing["socket"] = json!(socket);
        assert!(
            crate::tools::dispatch("phux_terminal_create", &missing)
                .await
                .is_err()
        );
        assert_eq!(
            count(&mut stream, &session).await,
            2,
            "refused placement must create no process or record"
        );
        let failure = json!({"idempotency_key":"55555555555555555555555555555555","session":session,"argv":[dir.path().join("missing-executable")]});
        let failed = invoke("phux_terminal_create", &socket, failure.clone()).await;
        assert_eq!(failed["execution"]["state"], "failed");
        assert_eq!(failed["execution"]["outcome"]["type"], "unobserved");
        assert!(failed["terminal_id"].is_null());
        let repeated_failure = invoke("phux_terminal_create", &socket, failure).await;
        assert_eq!(repeated_failure["resource"], failed["resource"]);
        assert_eq!(repeated_failure["replayed"], true);
        assert_eq!(repeated_failure["execution"]["state"], "failed");
        assert_eq!(count(&mut stream, &session).await, 3);
        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

async fn assert_legacy_layout(
    socket: &std::path::Path,
    stream: &mut tokio::net::UnixStream,
    session: &str,
    expected: &[ResourceId],
) {
    let page = organization(stream).await;
    let session = page
        .sessions
        .iter()
        .find(|record| record.key.to_string() == session)
        .unwrap()
        .live
        .unwrap();
    let mut conn = phux_client::attach::connection::Connection::connect(socket)
        .await
        .unwrap();
    let answer = conn
        .request_metadata(
            86,
            phux_protocol::wire::frame::Scope::Group(phux_protocol::ids::GroupId::new(1)),
            phux_protocol::wire::info::tui_layout_key(session),
        )
        .await
        .unwrap()
        .into_parts()
        .0;
    let document =
        phux_client::layout::ArrangementDoc::decode_cbor(&answer.unwrap().unwrap()).unwrap();
    let visible: Vec<_> = document
        .groups
        .iter()
        .flat_map(|group| {
            group
                .state
                .tree
                .as_ref()
                .map(phux_client::layout::leaves)
                .unwrap_or_default()
        })
        .collect();
    for id in expected {
        assert!(
            visible.contains(id),
            "new terminal missing from compatibility layout"
        );
    }
}

async fn organization(
    stream: &mut tokio::net::UnixStream,
) -> phux_protocol::wire::frame::OrganizationPage {
    let result = command(
        stream,
        81,
        Command::GetOrganization {
            filter: OrgFilter::All,
            page_size: 100,
            cursor: None,
        },
    )
    .await;
    let CommandResult::OkWith(CommandValue::OrganizationPage(page)) = result else {
        panic!("organization")
    };
    *page
}

async fn new_session(stream: &mut tokio::net::UnixStream, name: &str, op: u8) -> String {
    let result = command(
        stream,
        82,
        Command::SessionCreate {
            operation: OperationId::new([op; 16]).unwrap(),
            name: name.into(),
            lifetime: SessionLifetime::Persistent,
            attrs: vec![],
            launch: None,
        },
    )
    .await;
    let CommandResult::OkWith(CommandValue::Committed(receipt)) = result else {
        panic!("session")
    };
    receipt.session.unwrap().to_string()
}

async fn await_engine_exit(stream: &mut tokio::net::UnixStream, pane: u32) {
    tokio::time::timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            let result = command(
                stream,
                83,
                Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            )
            .await;
            let CommandResult::OkWith(CommandValue::State(state)) = result else {
                panic!("state")
            };
            if !state
                .resources
                .iter()
                .any(|resource| resource.id.local_id() == Some(pane))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn completed_records_survive_natural_exit_screen_expiry_and_server_restart() {
    use std::os::unix::fs::PermissionsExt as _;
    run_local(async {
        for timed_screen in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cwd = dir.path().join("execution");
            std::fs::create_dir(&cwd).unwrap();
            let program = cwd.join("relative-task");
            std::fs::write(&program, "#!/bin/sh\nexit 7\n").unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            let socket = dir.path().join("exit-api.sock");
            let store = dir.path().join("org.sqlite3");
            let (shutdown, server) =
                phux_server_testkit::spawn_server_with(socket.clone(), None, |config| {
                    config.seed_with_pty = true;
                    config.retain.by_default = timed_screen;
                    config.retain.default_secs = 1;
                    config.org_store = phux_server::OrgStoreLocation::Path(store.clone());
                });
            let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
            let session = new_session(&mut stream, "exit", 11).await;
            let request = json!({"idempotency_key":"66666666666666666666666666666666","session":session,"argv":["./relative-task"],"cwd":cwd});
            let created = invoke("phux_terminal_create", &socket, request.clone()).await;
            let terminal_id = u32::try_from(created["terminal_id"].as_u64().unwrap()).unwrap();
            await_engine_exit(&mut stream, terminal_id).await;
            let lookup = json!({"session":session,"idempotency_key":request["idempotency_key"]});
            let completed = invoke("phux_terminal_operation", &socket, lookup.clone()).await;
            assert_eq!(completed["execution"]["state"], "completed");
            assert_eq!(
                completed["execution"]["outcome"],
                json!({"type":"exited","code":7})
            );
            assert_eq!(count(&mut stream, &session).await, 1);
            drop(stream);
            join_after_shutdown(shutdown, server).await;
            std::fs::remove_dir_all(&cwd).unwrap();
            let (shutdown, server) =
                phux_server_testkit::spawn_server_with(socket.clone(), None, |config| {
                    config.seed_with_pty = true;
                    config.org_store = phux_server::OrgStoreLocation::Path(store.clone());
                });
            let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
            let recovered = invoke("phux_terminal_operation", &socket, lookup).await;
            assert_eq!(recovered["resource"], created["resource"]);
            assert_eq!(recovered["placement"], created["placement"]);
            assert_eq!(recovered["execution"]["outcome"]["code"], 7);
            let replay = invoke("phux_terminal_create", &socket, request).await;
            assert_eq!(replay["replayed"], true);
            assert_eq!(replay["resource"], created["resource"]);
            assert_eq!(count(&mut stream, &session).await, 1);
            let page = organization(&mut stream).await;
            let resource = page
                .resources
                .iter()
                .find(|record| record.key.to_string() == created["resource"].as_str().unwrap())
                .unwrap();
            let result = command(
                &mut stream,
                84,
                Command::ResourceRelaunch {
                    operation: OperationId::new([12; 16]).unwrap(),
                    resource: resource.key,
                    expected: resource.revision,
                },
            )
            .await;
            assert!(matches!(result, CommandResult::Accepted { .. }));
            let page = organization(&mut stream).await;
            assert_eq!(
                page.resources[0].activation.as_ref().unwrap().state,
                phux_protocol::wire::frame::ExecState::Failed
            );
            let old = invoke(
                "phux_terminal_operation",
                &socket,
                json!({"session":session,"idempotency_key":"66666666666666666666666666666666"}),
            )
            .await;
            assert!(
                old["execution"].is_null(),
                "old receipt cannot adopt replacement activation"
            );
            drop(stream);
            join_after_shutdown(shutdown, server).await;
        }
    });
}

#[test]
fn create_replay_and_lookup_do_not_observe_an_execution_moved_outside_origin() {
    run_local(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("move-api.sock");
        let (shutdown, server) = spawn_server_seed_pty_no_cmd(socket.clone(), None);
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let from = new_session(&mut stream, "from", 21).await;
        let to = new_session(&mut stream, "to", 22).await;
        let request = json!({"session":from,"idempotency_key":"77777777777777777777777777777777","argv":["/bin/sleep","30"]});
        let created = invoke("phux_terminal_create", &socket, request.clone()).await;
        let page = organization(&mut stream).await;
        let a = page
            .sessions
            .iter()
            .find(|session| session.key.to_string() == from)
            .unwrap();
        let b = page
            .sessions
            .iter()
            .find(|session| session.key.to_string() == to)
            .unwrap();
        let moved = command(
            &mut stream,
            85,
            Command::ResourceMove {
                operation: OperationId::new([23; 16]).unwrap(),
                resource: created["resource"].as_str().unwrap().parse().unwrap(),
                from: a.key,
                from_expected: a.revision,
                to: b.key,
                to_expected: b.revision,
            },
        )
        .await;
        assert!(matches!(
            moved,
            CommandResult::OkWith(CommandValue::Committed(_))
        ));
        for result in [
            invoke("phux_terminal_create", &socket, request).await,
            invoke(
                "phux_terminal_operation",
                &socket,
                json!({"session":from,"idempotency_key":"77777777777777777777777777777777"}),
            )
            .await,
        ] {
            assert_eq!(result["resource"], created["resource"]);
            assert!(result["execution"].is_null());
            assert!(result["terminal_id"].is_null());
        }
        assert_eq!(count(&mut stream, &to).await, 1);
        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn strict_request_rejects_unknown_and_private_environment_fields() {
    let base = json!({"idempotency_key":"11111111111111111111111111111111","session":"aebagbafaydqqcikbmga2dqpca"});
    let mut unknown = base.clone();
    unknown["silently_ignored"] = json!(true);
    assert!(serde_json::from_value::<phux_protocol::api::TerminalCreate>(unknown).is_err());
    let mut routing = base;
    routing["env"] = json!({"PHUX_SOCKET":"/production"});
    let request: phux_protocol::api::TerminalCreate = serde_json::from_value(routing).unwrap();
    assert!(request.command().is_err());
}
