/// Repair an already paired device without replacing its certificate, key,
/// or expiry, then prove repeats and rejected repairs do not rewrite authority.
#[test]
#[ignore = "runs the real binary; runs in the e2e lane"]
fn repair_paired_restores_control_without_reenrolling_or_widening_other_credentials() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    init_authority(dir.path());
    let phone = client_key();
    let certificate = dir.path().join("phone.pem");
    let id = enroll(
        dir.path(),
        &phone,
        &certificate,
        &["inventory,observe,create,bind,input,signal@global"],
    );
    let bot = client_key();
    let bot_id = enroll(
        dir.path(),
        &bot,
        &dir.path().join("bot.pem"),
        &["observe@host"],
    );
    let path = find(dir.path(), "workload-keys").expect("registry");
    let before: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let cert_before = std::fs::read(&certificate).unwrap();
    let out = phux(dir.path(), &["workload", "repair-paired", &id, "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_no_key_bytes(&out, &key_needles(&phone.key_pem));
    let document = json_doc(&out);
    assert_eq!(document["updated"], true);
    assert_eq!(document["credential_id"], id);
    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut expected = before["credentials"][0].clone();
    expected["scopes"] = serde_json::json!([phux_server::workload::PAIRED_DEVICE_SCOPE]);
    expected["principal"] = serde_json::json!("human");
    assert_eq!(after["credentials"][0], expected);
    assert_eq!(after["credentials"][1], before["credentials"][1]);
    assert_eq!(std::fs::read(&certificate).unwrap(), cert_before);
    let bytes = std::fs::read(&path).unwrap();
    let repeat = phux(dir.path(), &["workload", "repair-paired", &id]);
    assert!(repeat.status.success(), "{}", text(&repeat.stderr));
    assert!(text(&repeat.stdout).contains("Reconnect the device"));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    for target in [&bot_id, "sha256:bad"] {
        let refused = phux(dir.path(), &["workload", "repair-paired", target, "--json"]);
        assert!(!refused.status.success());
        let error: serde_json::Value = serde_json::from_slice(&refused.stderr).unwrap();
        assert!(error.get("error").is_some());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

/// Exercise the phone's actual mTLS/HELLO and command-dispatch boundary:
/// repair is not an implicit elevation of an already connected client.
#[test]
#[ignore = "spawns a real paired server; runs in the e2e lane"]
fn paired_repair_takes_effect_only_after_reconnect_on_the_same_certificate() {
    use phux_protocol::ids::{InputOperationId, OperationId};
    use phux_protocol::input::InputEvent;
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    use phux_protocol::kinds::Verb;
    use phux_protocol::wire::frame::{
        Command, CommandResult, CommandValue, Refusal, TerminateMode,
    };

    let dir = TempDir::new().unwrap();
    prepare_dirs(dir.path());
    paired(dir.path());
    init_authority(dir.path());
    let phone = client_key();
    let cert = dir.path().join("phone.pem");
    let key = dir.path().join("phone.key");
    std::fs::write(&key, &phone.key_pem).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let id = enroll(
        dir.path(),
        &phone,
        &cert,
        &["inventory,observe,create,bind,input,signal@global"],
    );
    let _server = start_server(dir.path(), &["--quic", listeners::LOOPBACK_ANY_PORT], &[]);
    let port = bound_quic_port(&dir.path().join("s.sock"));
    let mut original = RawSession::open_cert(port, &cert, &key);
    original.await_hello_ok();
    let operation = |byte| OperationId::new([byte; 16]).unwrap();
    let created = create_paired_phone_session(&mut original);
    let session = created.session.unwrap();
    let target = created.target.unwrap();
    let rename = |byte| Command::SessionRename {
        operation: operation(byte),
        session,
        name: "renamed".to_owned(),
    };
    let terminate = |byte| Command::ResourceTerminate {
        operation: operation(byte),
        target: target.clone(),
        mode: TerminateMode::Graceful,
    };
    let input = |byte| Command::ApplyInput {
        operation_id: InputOperationId::new([byte; 16]).unwrap(),
        terminal_id: target.resource.clone(),
        events: vec![InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Trusted,
            data: b"phone input\n".to_vec(),
        })],
    };
    for (request_id, command, expected) in [
        (
            2,
            rename(2),
            CommandResult::Refused(Refusal::Unauthorized {
                verb: Verb::Organize,
            }),
        ),
        (
            3,
            terminate(3),
            CommandResult::Refused(Refusal::Unauthorized {
                verb: Verb::Terminate,
            }),
        ),
        (4, input(4), CommandResult::Ok),
    ] {
        assert_eq!(paired_command(&mut original, request_id, command), expected);
    }
    let repaired = phux(dir.path(), &["workload", "repair-paired", &id, "--json"]);
    assert!(repaired.status.success(), "{}", text(&repaired.stderr));
    // The file's expanded ceiling does not mutate the original minted grant.
    assert_eq!(
        paired_command(&mut original, 5, rename(5)),
        CommandResult::Refused(Refusal::Unauthorized {
            verb: Verb::Organize
        })
    );
    let mut reconnected = RawSession::open_cert(port, &cert, &key);
    reconnected.await_hello_ok();
    for (request_id, command) in [(6, rename(6)), (7, input(7))] {
        let result = paired_command(&mut reconnected, request_id, command);
        assert!(
            matches!(
                result,
                CommandResult::Ok | CommandResult::OkWith(CommandValue::Committed(_))
            ),
            "{result:?}"
        );
    }
    assert_eq!(
        paired_command(&mut reconnected, 8, terminate(8)),
        CommandResult::Accepted {
            operation: operation(8)
        }
    );
    await_paired_phone_termination(&mut reconnected, &target.resource);
}

fn paired_command(
    live: &mut RawSession,
    request_id: u32,
    command: phux_protocol::wire::frame::Command,
) -> phux_protocol::wire::frame::CommandResult {
    live.send_frame(&phux_protocol::wire::frame::FrameKind::Command {
        request_id,
        command,
    });
    live.await_command_result(request_id)
}

fn create_paired_phone_session(live: &mut RawSession) -> phux_protocol::wire::frame::Committed {
    use phux_protocol::ids::OperationId;
    use phux_protocol::wire::frame::{
        Command, CommandResult, CommandValue, FrameKind, LaunchSpec, SessionLifetime,
    };
    live.send_frame(&FrameKind::Command {
        request_id: 1,
        command: Command::SessionCreate {
            operation: OperationId::new([1; 16]).unwrap(),
            name: "phone".to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: Vec::new(),
            launch: Some(Box::new(LaunchSpec::new(vec!["/bin/cat".to_owned()]))),
        },
    });
    match live.await_command_result(1) {
        CommandResult::OkWith(CommandValue::Committed(created)) => Ok(*created),
        other => Err(other),
    }
    .expect("paired create must succeed")
}

fn await_paired_phone_termination(
    live: &mut RawSession,
    terminal_id: &phux_protocol::ids::ResourceId,
) {
    use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};
    let deadline = Instant::now() + READY_DEADLINE;
    let mut request_id = 10;
    loop {
        live.send_frame(&FrameKind::Command {
            request_id,
            command: Command::GetTerminalState {
                terminal_id: terminal_id.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            },
        });
        let result = live.await_command_result(request_id);
        if matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                ..
            }
        ) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "pane did not terminate: {result:?}"
        );
        request_id += 1;
        std::thread::sleep(Duration::from_millis(25));
    }
}
