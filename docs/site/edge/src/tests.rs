use super::*;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::ids::GroupId;
use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{AttachTarget, SpawnResource, ViewportInfo};

// Exercise the same codec boundary and dispatcher as the WebSocket entry point.
fn exchange(session: &mut EdgeSession, frame: FrameKind) -> Vec<FrameKind> {
    let inbound = decode_inbound_frame(&encode(&frame)).expect("valid inbound frame");
    session
        .handle(inbound)
        .iter()
        .map(|bytes| {
            let (frame, rest) = FrameKind::decode(bytes).expect("valid server frame");
            assert!(rest.is_empty());
            frame
        })
        .collect()
}

fn attach(session: &mut EdgeSession) -> Vec<FrameKind> {
    exchange(
        session,
        FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::CreateIfMissing {
                name: "default".to_owned(),
                command: None,
                cwd: None,
            },
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: true,
            scrollback_limit_lines: 5000,
            role_policy: None,
        },
    )
}

fn spawn_request(request_id: u32) -> FrameKind {
    FrameKind::SpawnResource {
        request_id,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: Some(ResourceId::new(1)),
        agent_session: None,
        initial_size: Some((40, 24)),
        resource: None,
    }
}

fn spawn(session: &mut EdgeSession, request_id: u32) -> ResourceId {
    let reply = exchange(session, spawn_request(request_id));
    let [
        FrameKind::ResourceSpawned {
            request_id: actual,
            result: SpawnResult::Ok(id),
        },
    ] = reply.as_slice()
    else {
        panic!("expected one successful spawn reply, got {reply:?}");
    };
    assert_eq!(*actual, request_id);
    id.clone()
}

fn attach_resource(session: &mut EdgeSession, id: ResourceId) -> Vec<FrameKind> {
    exchange(
        session,
        FrameKind::Command {
            request_id: 7,
            command: Command::AttachResource {
                terminal_id: id,
                role_policy: None,
            },
        },
    )
}

fn key(
    session: &mut EdgeSession,
    id: ResourceId,
    physical: PhysicalKey,
    text: Option<&str>,
) -> Vec<FrameKind> {
    exchange(
        session,
        FrameKind::InputKey {
            terminal_id: id,
            event: KeyEvent {
                action: KeyAction::Press,
                key: physical,
                mods: ModSet::empty(),
                consumed_mods: ModSet::empty(),
                composing: false,
                text: text.map(str::to_owned),
                unshifted_codepoint: None,
            },
        },
    )
}

fn type_text(session: &mut EdgeSession, id: ResourceId, text: &str) -> Vec<FrameKind> {
    key(session, id, PhysicalKey::A, Some(text))
}

fn enter(session: &mut EdgeSession, id: ResourceId) -> Vec<FrameKind> {
    key(session, id, PhysicalKey::Enter, None)
}

fn vt(frames: &[FrameKind]) -> String {
    frames
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::ResourceOutput { bytes, .. } => Some(String::from_utf8_lossy(bytes)),
            FrameKind::BootstrapChunk { payload, .. } => Some(String::from_utf8_lossy(payload)),
            _ => None,
        })
        .collect()
}

fn resize(session: &mut EdgeSession, id: ResourceId, cols: u16, rows: u16) -> Vec<FrameKind> {
    exchange(
        session,
        FrameKind::ResizeTerminal {
            terminal_id: id,
            cols,
            rows,
            cell_px: None,
        },
    )
}

fn kill(session: &mut EdgeSession, id: ResourceId) -> Vec<FrameKind> {
    exchange(
        session,
        FrameKind::Command {
            request_id: 9,
            command: Command::KillResource {
                terminal_id: id,
                operation_id: None,
            },
        },
    )
}

#[test]
fn hello_negotiates_raw_vt_and_initial_spawn_size() {
    let mut session = EdgeSession::new(80, 24, "demo", "");
    let response = exchange(
        &mut session,
        FrameKind::Hello {
            client_name: "edge-tests".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new(),
        },
    );
    let [
        FrameKind::HelloOk {
            selected_profile,
            server_caps,
            ..
        },
    ] = response.as_slice()
    else {
        panic!("HELLO_OK missing")
    };
    assert_eq!(*selected_profile, BootstrapProfile::SynthesizedVtRaw);
    assert!(
        server_caps
            .features
            .contains(ServerFeature::SpawnInitialSize)
    );
}

#[test]
fn distinct_shells_survive_resize_then_close_without_cross_talk() {
    let mut session = EdgeSession::new(80, 24, "demo", "");
    let one = ResourceId::new(1);
    let first = attach(&mut session);
    assert!(matches!(
        first.as_slice(),
        [
            FrameKind::Attached { .. },
            FrameKind::BootstrapBegin { .. },
            FrameKind::BootstrapChunk { .. },
            FrameKind::BootstrapReady { .. },
            FrameKind::AttachReady { .. }
        ]
    ));
    type_text(&mut session, one.clone(), "echo first-pane");
    let two = spawn(&mut session, 42);
    let second = attach_resource(&mut session, two.clone());
    assert!(matches!(
        second.as_slice(),
        [
            FrameKind::BootstrapBegin { .. },
            FrameKind::BootstrapChunk { .. },
            FrameKind::BootstrapReady { .. },
            FrameKind::CommandResult {
                request_id: 7,
                result: CommandResult::Ok
            }
        ]
    ));
    type_text(&mut session, two.clone(), "echo second-pane");
    let resized = resize(&mut session, one.clone(), 39, 24);
    assert!(vt(&resized).contains("echo first-pane"));
    assert!(!vt(&resized).contains("second-pane"));
    let first_output = enter(&mut session, one.clone());
    assert!(vt(&first_output).contains("\r\nfirst-pane\r\n"));
    let second_output = enter(&mut session, two.clone());
    assert!(vt(&second_output).contains("\r\nsecond-pane\r\n"));
    assert!(!vt(&second_output).contains("first-pane"));
    let closed = kill(&mut session, two.clone());
    assert!(
        matches!(closed.as_slice(), [FrameKind::CommandResult { request_id: 9, result: CommandResult::Ok }, FrameKind::ResourceClosed { terminal_id, reason: CloseReason::Killed, .. }] if terminal_id == &two)
    );
    type_text(&mut session, one.clone(), "echo still-live");
    assert!(vt(&enter(&mut session, one)).contains("\r\nstill-live\r\n"));
    assert_eq!(spawn(&mut session, 43), ResourceId::new(3));
}

#[test]
fn replay_generations_and_sequences_are_per_terminal_and_never_reset() {
    let mut session = EdgeSession::new(80, 24, "demo", "");
    let one = ResourceId::new(1);
    attach(&mut session);
    let two = spawn(&mut session, 2);
    attach_resource(&mut session, two.clone());
    let first = type_text(&mut session, one.clone(), "one");
    let second = type_text(&mut session, two.clone(), "two");
    let [
        FrameKind::ResourceOutput {
            stream_id: stream_one,
            bootstrap_id: old_generation,
            seq: 1,
            ..
        },
    ] = first.as_slice()
    else {
        panic!("first output")
    };
    let [
        FrameKind::ResourceOutput {
            stream_id: stream_two,
            seq: 1,
            ..
        },
    ] = second.as_slice()
    else {
        panic!("second output")
    };
    assert_ne!(stream_one, stream_two);
    let resized = resize(&mut session, one.clone(), 38, 12);
    let FrameKind::BootstrapBegin {
        stream_id,
        bootstrap_id: generation,
        base_seq: 1,
        cols: 38,
        rows: 12,
        ..
    } = &resized[0]
    else {
        panic!("resize bootstrap")
    };
    assert_eq!(stream_id, stream_one);
    assert!(generation > old_generation);
    let after = type_text(&mut session, one, "!");
    assert!(
        matches!(after.as_slice(), [FrameKind::ResourceOutput { bootstrap_id, seq: 2, .. }] if bootstrap_id == generation)
    );
    let sibling = type_text(&mut session, two, "!");
    assert!(
        matches!(sibling.as_slice(), [FrameKind::ResourceOutput { bootstrap_id, seq: 2, .. }] if bootstrap_id == old_generation)
    );
}

#[test]
fn hibernation_restores_all_shells_output_generations_and_allocator() {
    let mut session = EdgeSession::new(80, 24, "demo", "");
    let one = ResourceId::new(1);
    attach(&mut session);
    let two = spawn(&mut session, 2);
    attach_resource(&mut session, two.clone());
    let discarded = spawn(&mut session, 3);
    kill(&mut session, discarded);
    type_text(&mut session, one.clone(), "echo retained-one");
    enter(&mut session, one.clone());
    type_text(&mut session, one.clone(), "echo partial-one");
    type_text(&mut session, two.clone(), "echo partial-two");
    let checkpoint = session.checkpoint();
    let mut restored = EdgeSession::restore_inner(&checkpoint, "demo", "").unwrap();
    assert_eq!(restored.checkpoint(), checkpoint);
    // Hibernation resumes output on the existing subscription without an ATTACH.
    assert!(vt(&enter(&mut restored, two.clone())).contains("\r\npartial-two\r\n"));
    let reattached = attach(&mut restored);
    let FrameKind::Attached { snapshot, .. } = &reattached[0] else {
        panic!("snapshot missing")
    };
    assert_eq!(
        snapshot
            .resources
            .iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>(),
        vec![one.clone(), two]
    );
    assert!(vt(&reattached).contains("retained-one"));
    assert!(vt(&reattached).contains("echo partial-one"));
    assert!(vt(&enter(&mut restored, one)).contains("\r\npartial-one\r\n"));
    assert_eq!(spawn(&mut restored, 4), ResourceId::new(4));
}

#[test]
fn refusals_do_not_mutate_or_kill_live_resources() {
    let mut session = EdgeSession::new(80, 24, "demo", "");
    attach(&mut session);
    for id in 2..=4 {
        spawn(&mut session, id);
    }
    let before = session.checkpoint();
    assert!(matches!(
        exchange(&mut session, spawn_request(5)).as_slice(),
        [FrameKind::ResourceSpawned {
            request_id: 5,
            result: SpawnResult::Err(SpawnError::SpawnFailed(_))
        }]
    ));
    for id in [
        ResourceId::new(0),
        ResourceId::new(99),
        ResourceId::satellite("elsewhere", 1),
    ] {
        assert!(matches!(
            type_text(&mut session, id.clone(), "bad").as_slice(),
            [FrameKind::Error { .. }]
        ));
        assert!(matches!(
            resize(&mut session, id.clone(), 40, 24).as_slice(),
            [FrameKind::Error { .. }]
        ));
        assert!(matches!(
            attach_resource(&mut session, id.clone()).as_slice(),
            [FrameKind::CommandResult {
                result: CommandResult::Error { .. },
                ..
            }]
        ));
        assert!(matches!(
            kill(&mut session, id).as_slice(),
            [FrameKind::CommandResult {
                result: CommandResult::Error { .. },
                ..
            }]
        ));
    }
    assert!(matches!(
        resize(&mut session, ResourceId::new(1), 1001, 24).as_slice(),
        [FrameKind::Error {
            code: ErrorCode::MalformedMessage,
            ..
        }]
    ));
    assert_eq!(session.checkpoint(), before);
}

#[test]
fn unsupported_spawn_and_last_close_are_correlated_refusals() {
    let mut session = EdgeSession::new(80, 24, "demo", "");
    let before = session.checkpoint();
    let mut unsupported = spawn_request(77);
    if let FrameKind::SpawnResource { command, .. } = &mut unsupported {
        *command = Some(vec!["sh".to_owned()]);
    }
    assert!(matches!(
        exchange(&mut session, unsupported).as_slice(),
        [FrameKind::ResourceSpawned {
            request_id: 77,
            result: SpawnResult::Err(SpawnError::SpawnFailed(_))
        }]
    ));
    let mut agent = spawn_request(78);
    if let FrameKind::SpawnResource {
        resource,
        owner_terminal,
        initial_size,
        ..
    } = &mut agent
    {
        *owner_terminal = None;
        *initial_size = None;
        *resource = Some(Box::new(SpawnResource::agent_session(
            ResourceId::new(1),
            "claude",
        )));
    }
    assert!(matches!(
        exchange(&mut session, agent).as_slice(),
        [FrameKind::ResourceSpawned {
            request_id: 78,
            result: SpawnResult::Err(SpawnError::UnsupportedKind)
        }]
    ));
    assert!(matches!(
        kill(&mut session, ResourceId::new(1)).as_slice(),
        [FrameKind::CommandResult {
            request_id: 9,
            result: CommandResult::Error {
                code: ErrorCode::PreconditionFailed,
                ..
            }
        }]
    ));
    assert_eq!(session.checkpoint(), before);
}

#[test]
fn addressed_paste_and_input_limits_preserve_partial_lines_atomically() {
    let mut session = EdgeSession::new(80, 24, "demo", "");
    attach(&mut session);
    let two = spawn(&mut session, 2);
    attach_resource(&mut session, two.clone());
    type_text(&mut session, ResourceId::new(1), "echo first");
    let pasted = exchange(
        &mut session,
        FrameKind::InputPaste {
            terminal_id: two.clone(),
            event: PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"echo second\r\necho pending".to_vec(),
            },
        },
    );
    assert!(vt(&pasted).contains("\r\nsecond\r\n"));
    let before = session.checkpoint();
    for data in [vec![b'x'; 4097], b"oops\x1b[2J".to_vec(), vec![0xff]] {
        assert!(matches!(
            exchange(
                &mut session,
                FrameKind::InputPaste {
                    terminal_id: two.clone(),
                    event: PasteEvent {
                        trust: PasteTrust::Untrusted,
                        data
                    }
                }
            )
            .as_slice(),
            [FrameKind::Error { .. }]
        ));
    }
    assert!(matches!(
        type_text(&mut session, two.clone(), &"x".repeat(4096)).as_slice(),
        [FrameKind::Error {
            code: ErrorCode::CanonicalLimitExceeded,
            ..
        }]
    ));
    assert_eq!(session.checkpoint(), before);
    assert!(vt(&enter(&mut session, two)).contains("\r\npending\r\n"));
    assert!(vt(&enter(&mut session, ResourceId::new(1))).contains("\r\nfirst\r\n"));
}

#[test]
fn v1_migration_preserves_partial_input_and_sequence() {
    let old = r#"{"version":1,"kind":"edge-session","cols":80,"rows":24,"seq":19,"shell":{"kind":"demo","line":"echo migrated"}}"#;
    let mut restored = EdgeSession::restore_inner(old, "demo", "").unwrap();
    let output = enter(&mut restored, ResourceId::new(1));
    assert!(matches!(
        output.as_slice(),
        [FrameKind::ResourceOutput { seq: 20, .. }]
    ));
    assert!(vt(&output).contains("\r\nmigrated\r\n"));
    assert_eq!(spawn(&mut restored, 1), ResourceId::new(2));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&restored.checkpoint()).unwrap()["version"],
        2
    );
}

#[test]
fn checkpoint_restore_rejects_corruption_instead_of_starting_a_new_shell() {
    let valid = EdgeSession::new(80, 24, "demo", "").checkpoint();
    let state: serde_json::Value = serde_json::from_str(&valid).unwrap();
    let mut invalid = Vec::new();
    for (key, value) in [
        ("id", serde_json::json!(0)),
        ("cols", serde_json::json!(0)),
        ("rows", serde_json::json!(1001)),
        ("seq", serde_json::json!(u64::MAX)),
        ("generation", serde_json::json!(u64::MAX)),
        ("transcript", serde_json::json!("x".repeat(8193))),
    ] {
        let mut copy = state.clone();
        copy["terminals"][0][key] = value;
        invalid.push(copy);
    }
    let mut duplicate = state.clone();
    duplicate["terminals"]
        .as_array_mut()
        .unwrap()
        .push(state["terminals"][0].clone());
    invalid.push(duplicate);
    let mut allocator = state.clone();
    allocator["next_id"] = serde_json::json!(1);
    invalid.push(allocator);
    let mut version = state.clone();
    version["version"] = serde_json::json!(3);
    invalid.push(version);
    let mut extra = state.clone();
    extra["unknown"] = serde_json::json!(true);
    invalid.push(extra);
    let mut many = state.clone();
    many["terminals"] = serde_json::json!(vec![state["terminals"][0].clone(); 5]);
    invalid.push(many);
    for invalid in invalid {
        assert!(EdgeSession::restore_inner(&invalid.to_string(), "demo", "").is_err());
    }
    assert!(EdgeSession::restore_inner(&" ".repeat(MAX_CHECKPOINT_BYTES + 1), "demo", "").is_err());
    assert!(EdgeSession::restore_inner(&valid, "portfolio", "{}").is_err());
    assert!(EdgeSession::restore_inner(&valid, "invalid", "").is_err());
    let bad_legacy = r#"{"version":1,"kind":"edge-session","cols":80,"rows":24,"seq":0,"extra":true,"shell":{"kind":"demo","line":""}}"#;
    assert!(EdgeSession::restore_inner(bad_legacy, "demo", "").is_err());
}

#[test]
fn retained_output_and_checkpoint_remain_bounded_with_four_busy_shells() {
    let mut session = EdgeSession::new(80, 24, "demo", "");
    attach(&mut session);
    let mut ids = vec![ResourceId::new(1)];
    for id in 2..=4 {
        ids.push(spawn(&mut session, id));
    }
    for id in &ids {
        attach_resource(&mut session, id.clone());
        for _ in 0..30 {
            type_text(
                &mut session,
                id.clone(),
                &format!("echo {}", "\\\"".repeat(1800)),
            );
            enter(&mut session, id.clone());
        }
        type_text(&mut session, id.clone(), "echo latest-partial");
    }
    let checkpoint = session.checkpoint();
    assert!(checkpoint.len() < MAX_CHECKPOINT_BYTES);
    let mut restored = EdgeSession::restore_inner(&checkpoint, "demo", "").unwrap();
    for id in ids {
        let replay = resize(&mut restored, id.clone(), 30, 12);
        assert!(vt(&replay).contains("echo latest-partial"));
        assert!(vt(&enter(&mut restored, id)).contains("\r\nlatest-partial\r\n"));
    }
}

#[test]
fn decoder_rejects_compression_trailing_frames_and_oversized_envelopes() {
    let frame = encode(&spawn_request(1));
    assert!(decode_inbound_frame(&frame).is_some());
    let mut compressed = frame.clone();
    compressed[4] = TYPE_FRAME_COMPRESSED;
    assert!(decode_inbound_frame(&compressed).is_none());
    let mut trailing = frame.clone();
    trailing.extend(&frame);
    assert!(decode_inbound_frame(&trailing).is_none());
    assert!(decode_inbound_frame(&vec![0; MAX_INBOUND_BYTES + 1]).is_none());
}

#[test]
fn closing_the_initial_resource_does_not_make_resource_one_special() {
    let mut session = EdgeSession::new(80, 24, "demo", "");
    attach(&mut session);
    let two = spawn(&mut session, 2);
    attach_resource(&mut session, two.clone());
    type_text(&mut session, two.clone(), "echo survivor");
    kill(&mut session, ResourceId::new(1));
    let mut restored = EdgeSession::restore_inner(&session.checkpoint(), "demo", "").unwrap();
    let attached = attach(&mut restored);
    assert!(
        matches!(&attached[0], FrameKind::Attached { snapshot, .. } if snapshot.focused_resource == two && snapshot.resources.len() == 1)
    );
    assert!(vt(&enter(&mut restored, two.clone())).contains("\r\nsurvivor\r\n"));
    let mut request = spawn_request(3);
    if let FrameKind::SpawnResource { owner_terminal, .. } = &mut request {
        *owner_terminal = Some(two);
    }
    assert!(
        matches!(exchange(&mut restored, request).as_slice(), [FrameKind::ResourceSpawned { result: SpawnResult::Ok(id), .. }] if id == &ResourceId::new(3))
    );
}

#[test]
fn long_line_edit_history_compacts_without_losing_current_input() {
    let mut session = EdgeSession::new(80, 24, "demo", "");
    attach(&mut session);
    for _ in 0..2200 {
        type_text(&mut session, ResourceId::new(1), "x");
        key(
            &mut session,
            ResourceId::new(1),
            PhysicalKey::Backspace,
            None,
        );
    }
    type_text(&mut session, ResourceId::new(1), "echo edited");
    let mut restored = EdgeSession::restore_inner(&session.checkpoint(), "demo", "").unwrap();
    assert!(vt(&resize(&mut restored, ResourceId::new(1), 40, 12)).contains("echo edited"));
    assert!(vt(&enter(&mut restored, ResourceId::new(1))).contains("\r\nedited\r\n"));
}

#[test]
fn portfolio_selection_and_detail_survive_hibernation_independently() {
    let repos = ["one", "two"].map(|name| {
        serde_json::json!({
            "name": name, "url": format!("https://example.com/{name}"), "stars": 0,
            "forks": 0, "open_issues": 0, "pushed_at": ""
        })
    });
    let snapshot = serde_json::json!({"repos": repos}).to_string();
    let mut session = EdgeSession::new(80, 24, "portfolio", &snapshot);
    attach(&mut session);
    let two = spawn(&mut session, 2);
    attach_resource(&mut session, two.clone());
    key(&mut session, two.clone(), PhysicalKey::ArrowDown, None);
    enter(&mut session, two.clone());
    let mut restored =
        EdgeSession::restore_inner(&session.checkpoint(), "portfolio", &snapshot).unwrap();
    let first = enter(&mut restored, ResourceId::new(1));
    let second = resize(&mut restored, two, 39, 24);
    assert!(vt(&first).contains("https://example.com/one"));
    assert!(!vt(&first).contains("https://example.com/two"));
    assert!(vt(&second).contains("https://example.com/two"));
    assert!(!vt(&second).contains("https://example.com/one"));
    let legacy = r#"{"version":1,"kind":"edge-session","cols":80,"rows":24,"seq":0,"shell":{"kind":"portfolio","selected":1,"detail":true}}"#;
    let mut migrated = EdgeSession::restore_inner(legacy, "portfolio", &snapshot).unwrap();
    assert!(vt(&attach(&mut migrated)).contains("https://example.com/two"));
}
