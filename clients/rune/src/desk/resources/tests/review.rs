//! Regression cases from the independent resource-pane review.
use super::*;

fn window(client: &Arc<Client>) -> Runtime<Desk> {
    let mut desk = Desk::new(&Resolved::offline("review fixture"));
    desk.link.heard = true;
    desk.client = Some(Arc::clone(client));
    desk.resources.images = Some(rune::ImageCache::new(Arc::new(|| {})));
    let mut rt = Runtime::new(desk, crate::ui::view, (800.0, 600.0), Env::new(true, 1.0));
    rt.on_key = Some(Desk::key);
    rt
}

fn attached(client: &Client, attach_id: u32, records: &[ResourceRecord]) {
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), id(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(session())),
        ])
        .with_resources(
            records
                .iter()
                .map(|r| {
                    ResourceInfo::resource(r.live.clone().unwrap(), r.kind)
                        .with_session(Some(SessionId::new(1)))
                })
                .collect(),
        );
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
}

#[test]
fn one_observation_with_old_and_new_publications_uses_latest_incarnation_only() {
    for include_lost in [true, false] {
        let (client, mut rt) = fixture(
            &[record(1, ResourceKind::AccessibilityTree)],
            &doc(LayoutNode::Leaf(id(1))),
        );
        publish(
            &client,
            1,
            ResourceKind::AccessibilityTree,
            b"{\"old\":true}",
        );
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: id(1),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq: 42,
                bytes: b"{\"stale_high_seq\":true}".to_vec().into(),
            })
            .unwrap();
        // Do not drain the UI at any intermediate lifecycle stage.
        client.with_control(|c| {
            if include_lost {
                c.connection_lost(None);
            }
            c.connection_opened();
        });
        outbound(&client);
        client.feed(hello(2)).unwrap();
        let records = [record(2, ResourceKind::AccessibilityTree).with_live(Some(id(1)))];
        let attach_id = seed(&client, &records).unwrap();
        install_arrangement(&client, &records, &doc(LayoutNode::Leaf(id(1))));
        attached(&client, attach_id, &records);
        publish(
            &client,
            1,
            ResourceKind::AccessibilityTree,
            b"{\"latest\":true}",
        );
        client.feed(FrameKind::AttachReady { attach_id }).unwrap();
        let observation = client.take_activity();
        let wakes = observation.events.iter().filter(|e| matches!(e, phux_client_runtime::control::Event::DocumentPublished { resource_id, .. } if *resource_id == id(1))).count();
        assert_eq!(wakes, 1, "the newest wake supersedes the stale one");
        assert_eq!(
            client.acquire_document(&id(1)).unwrap().bytes,
            b"{\"latest\":true}"
        );
        assert_eq!(
            observation.events.iter().any(|e| matches!(
                e,
                phux_client_runtime::control::Event::ConnectionLost { .. }
            )),
            include_lost
        );
        rt.update(|d, tx| {
            d.observe(&client, observation, tx);
        }); // ONE observation owns all stages.
        let document = &rt.state.resources.entries[&id(1)];
        assert_eq!(document.record.key, key(2));
        assert_eq!(
            document.seq,
            Some(0),
            "old high sequence must not poison the new id"
        );
        assert!(
            matches!(&document.body, Body::Tree(json) if json.contains("latest") && !json.contains("old"))
        );
        step(&mut rt);
        assert_eq!(rt.state.shown.as_ref().unwrap().resource, id(1));
    }
}

#[test]
fn first_connection_cut_before_attach_ready_survives_server_identity_establishment() {
    let client = Arc::new(ClientRuntime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("work".to_owned())),
        ..ControlOptions::default()
    }));
    client.with_control(ControlPlane::connection_opened);
    outbound(&client);
    let mut rt = window(&client);
    step(&mut rt); // epoch is known; server id is not yet known.
    client.feed(hello(1)).unwrap();
    let records = [record(1, ResourceKind::AccessibilityTree)];
    let attach_id = seed(&client, &records).unwrap();
    install_arrangement(&client, &records, &doc(LayoutNode::Leaf(id(1))));
    attached(&client, attach_id, &records);
    publish(
        &client,
        1,
        ResourceKind::AccessibilityTree,
        b"{\"first_cut\":true}",
    );
    step(&mut rt); // drain BootstrapReady, before the first AttachReady.
    assert_eq!(rt.state.resources.pending.len(), 1);
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
    step(&mut rt); // no second publication is sent.
    assert!(
        matches!(&rt.state.resources.entries[&id(1)].body, Body::Tree(json) if json.contains("first_cut"))
    );
}

fn foreign_fixture() -> (Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::AccessibilityTree)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    let foreign = SessionKey::new([2; 16]).unwrap();
    let mut second = record(2, ResourceKind::AccessibilityTree);
    second.session = foreign;
    let request = client.refresh_organization().unwrap();
    outbound(&client);
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(2), 0)
        .with_sessions(vec![
            SessionRecord::new(session(), "work", Revision::new(1))
                .with_live(Some(SessionId::new(1)))
                .with_arrangement(Some(arrangement())),
            SessionRecord::new(foreign, "foreign", Revision::new(1))
                .with_live(Some(SessionId::new(2))),
        ])
        .with_resources(vec![record(1, ResourceKind::AccessibilityTree), second]);
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .unwrap();
    // GET_STATE advertises B's session/resource without subscribing its pump.
    // The home ATTACHED snapshot above intentionally contained only A.
    let refresh = client.with_control(ControlPlane::refresh_topology).unwrap();
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), id(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(session())),
            SessionInfo::new(SessionId::new(2), "foreign").with_key(Some(foreign)),
        ])
        .with_resources(vec![
            ResourceInfo::resource(id(1), ResourceKind::AccessibilityTree)
                .with_session(Some(SessionId::new(1))),
            ResourceInfo::resource(id(2), ResourceKind::AccessibilityTree)
                .with_session(Some(SessionId::new(2))),
        ]);
    client
        .feed(FrameKind::CommandResult {
            request_id: refresh,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        })
        .unwrap();
    step(&mut rt);
    outbound(&client);
    (client, rt)
}

fn open_foreign(client: &Client, rt: &mut Runtime<Desk>) -> u32 {
    let row = rt.state.palette_hits().iter().position(|hit| matches!(&hit.item.target,
        crate::palette::Target::Terminal { id: target, session } if *target == id(2) && session == "foreign")).expect("foreign document is discoverable in A’s palette");
    rt.update(|d, tx| d.palette_run(Some(row), tx));
    assert_eq!(client.selected_session(), Some(2));
    let frames = outbound(client);
    assert!(
        !frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::ResizeTerminal { .. }))
    );
    let subscriptions: Vec<_> = frames
        .into_iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command:
                    Command::AttachResource {
                        terminal_id,
                        role_policy,
                        ..
                    },
            } => Some((request_id, terminal_id, role_policy)),
            _ => None,
        })
        .collect();
    assert_eq!(
        subscriptions.len(),
        1,
        "opening must request exactly one foreign resource pump"
    );
    let (request, target, role) = &subscriptions[0];
    assert_eq!(target, &id(2));
    assert_eq!(*role, Some(phux_protocol::wire::frame::RolePolicy::VIEWER));
    assert_eq!(rt.state.shown.as_ref().unwrap().resource, id(2));
    assert!(rt.state.shown.as_ref().unwrap().view.is_none());
    *request
}

#[test]
fn opening_never_viewed_foreign_document_subscribes_and_adopts_existing_cut() {
    let (client, mut rt) = foreign_fixture();
    let request = open_foreign(&client, &mut rt);
    step(&mut rt);
    assert!(
        outbound(&client).is_empty(),
        "pending subscription is not repeated"
    );
    publish(
        &client,
        2,
        ResourceKind::AccessibilityTree,
        b"{\"existing_B\":true}",
    );
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::Ok,
        })
        .unwrap();
    step(&mut rt);
    assert!(
        matches!(&rt.state.resources.entries[&id(2)].body, Body::Tree(json) if json.contains("existing_B"))
    );
    assert!(rt.state.shown.as_ref().unwrap().view.is_none());
    client.with_control(|control| control.resize_viewport(90, 30));
    assert!(!outbound(&client).iter().any(|frame| matches!(frame, FrameKind::ResizeTerminal { terminal_id, .. } if *terminal_id == id(2))), "viewer subscription stays outside terminal geometry voting");
}

#[test]
fn foreign_subscription_refusal_is_visible_and_only_explicit_reopen_retries() {
    let (client, mut rt) = foreign_fixture();
    let request = open_foreign(&client, &mut rt);
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::Error {
                code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
                message: "viewer refused".to_owned(),
            },
        })
        .unwrap();
    step(&mut rt);
    assert!(
        matches!(&rt.state.resources.entries[&id(2)].body, Body::Invalid(text) if text.contains("viewer refused"))
    );
    assert!(
        outbound(&client).is_empty(),
        "refusal does not cause a retry loop"
    );
    let retried = open_foreign(&client, &mut rt);
    assert_ne!(retried, request);
    publish(
        &client,
        2,
        ResourceKind::AccessibilityTree,
        b"{\"retried\":true}",
    );
    client
        .feed(FrameKind::CommandResult {
            request_id: retried,
            result: CommandResult::Ok,
        })
        .unwrap();
    step(&mut rt);
    assert!(
        matches!(&rt.state.resources.entries[&id(2)].body, Body::Tree(json) if json.contains("retried"))
    );
}

#[test]
fn native_json_selection_and_copy_shortcuts_copy_plain_text_without_remote_input() {
    let (client, mut rt) = fixture(
        &[
            record(1, ResourceKind::PixelSurface),
            record(2, ResourceKind::AccessibilityTree),
        ],
        &doc(split(1, 2)),
    );
    publish(
        &client,
        2,
        ResourceKind::AccessibilityTree,
        br#"{"name":"plain <b>JSON</b>"}"#,
    );
    step(&mut rt);
    let clipboard = rune::clipboard::memory();
    rt.use_clipboard(&clipboard);
    outbound(&client);
    let key = text_key(&id(2));
    let rect = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key.as_str()))
        .unwrap()
        .rect;
    rt.pointer_down(rect[0] + 5.0, rect[1] + 5.0);
    rt.pointer_up(rect[0] + 5.0, rect[1] + 5.0);
    rt.frame(0.0);
    assert_eq!(
        rt.state.shown.as_ref().unwrap().resource,
        id(2),
        "selecting a secondary JSON pane gives it keyboard focus"
    );
    for command in ['a', 'c'] {
        rt.key(KeyEvent {
            key: Key::Command(command),
            mods: Mods {
                cmd: true,
                ..Mods::default()
            },
        });
        rt.frame(0.0);
    }
    let Body::Tree(json) = &rt.state.resources.entries[&id(2)].body else {
        panic!("native JSON")
    };
    assert_eq!(rt.selected_text().as_deref(), Some(json.as_str()));
    assert_eq!(
        clipboard.text().map(|text| text.to_string()),
        Some(json.clone())
    );
    assert!(
        json.contains("<b>JSON</b>"),
        "copied as authored plain text, not interpreted markup"
    );
    let expected = json.clone();
    rt.update(|_, tx| {
        tx.clear_text_selection();
        tx.copy("sentinel");
    });
    rt.update(|d, tx| d.shortcut(crate::keys::Shortcut::SelectAll, tx));
    rt.frame(0.0);
    rt.update(|d, tx| d.shortcut(crate::keys::Shortcut::Copy, tx));
    assert_eq!(
        clipboard.text().map(|text| text.to_string()),
        Some(expected),
        "menu/palette actions also use the local text selection"
    );
    assert!(
        outbound(&client).is_empty(),
        "copy/select all do not address a terminal"
    );
}

#[test]
fn dragging_a_secondary_json_selection_also_focuses_its_resource() {
    let (client, mut rt) = fixture(
        &[
            record(1, ResourceKind::PixelSurface),
            record(2, ResourceKind::AccessibilityTree),
        ],
        &doc(split(1, 2)),
    );
    publish(
        &client,
        2,
        ResourceKind::AccessibilityTree,
        br#"{"name":"dragging native JSON selection"}"#,
    );
    step(&mut rt);
    outbound(&client);
    let key = text_key(&id(2));
    let rect = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key.as_str()))
        .unwrap()
        .rect;
    rt.pointer_down(rect[0] + 1.0, rect[1] + 5.0);
    rt.pointer_move(rect[0] + 100.0, rect[1] + 20.0);
    rt.frame(0.0);
    assert!(
        rt.selected_text().is_some(),
        "a real native text drag selected characters"
    );
    assert_eq!(rt.state.shown.as_ref().unwrap().resource, id(2));
    rt.pointer_up(rect[0] + 100.0, rect[1] + 20.0);
    rt.frame(0.0);
    assert!(outbound(&client).is_empty());
}

#[test]
fn a_stale_document_wake_reads_nothing_and_the_newer_wake_applies() {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::AccessibilityTree)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    publish(&client, 1, ResourceKind::AccessibilityTree, b"{\"cut\":0}");
    step(&mut rt);
    client
        .feed(FrameKind::ResourceOutput {
            terminal_id: id(1),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 1,
            bytes: b"{\"live\":1}".to_vec().into(),
        })
        .unwrap();
    // The cut's wake, drained after the slot moved on: it must not read the
    // live document, whose own wake follows.
    let stale = phux_client_runtime::control::Event::DocumentPublished {
        resource_id: id(1),
        generation: 1,
    };
    let mut applied = true;
    rt.update(|d, tx| applied = d.apply(&client, stale, tx));
    assert!(!applied, "a stale wake changes nothing");
    assert!(
        matches!(&rt.state.resources.entries[&id(1)].body, Body::Tree(json) if json.contains("cut"))
    );
    step(&mut rt);
    assert!(
        matches!(&rt.state.resources.entries[&id(1)].body, Body::Tree(json) if json.contains("live"))
    );
}
