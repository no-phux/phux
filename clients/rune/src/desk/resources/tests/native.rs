use super::*;

#[test]
fn bootstrap_before_identity_is_adopted_after_organization_seed() {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::AccessibilityTree)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    rt.state.resources.entries.clear();
    let publication = Arc::new(DocumentPublication {
        resource_id: id(1),
        kind: ResourceKind::AccessibilityTree,
        seq: 7,
        bytes: b"{\"early\":true}".to_vec(),
    });
    assert!(!rt.state.publish_document(&publication));
    assert_eq!(rt.state.resources.pending.len(), 1);
    assert!(rt.state.refresh_resources(&client));
    assert_eq!(rt.state.resources.entries[&id(1)].seq, Some(7));
    assert!(rt.state.resources.pending.is_empty());
    let unknown = Arc::new(DocumentPublication {
        resource_id: id(99),
        kind: ResourceKind::AccessibilityTree,
        seq: 2,
        bytes: b"{}".to_vec(),
    });
    rt.state.publish_document(&unknown);
    rt.state.refresh_resources(&client);
    assert!(
        rt.state.resources.pending.contains_key(&id(99)),
        "a publication can beat a live inventory refetch, not only the first seed"
    );
    rt.state.resources.pending.get_mut(&id(99)).unwrap().since =
        Instant::now() - crate::desk::WANTED_GRACE;
    rt.state.refresh_resources(&client);
    assert!(
        !rt.state.resources.pending.contains_key(&id(99)),
        "unknown identity does not retain bytes forever"
    );
}

#[test]
fn metadata_rename_preserves_document_but_a_new_execution_does_not() {
    let original = record(1, ResourceKind::AccessibilityTree);
    assert!(same_execution(
        &original,
        &original.clone().with_name(Some("renamed".to_owned()))
    ));
    let activation = phux_protocol::wire::frame::ActivationInfo::new(
        phux_protocol::ids::ActivationId::new(1),
        phux_protocol::ids::ServerInstance::new([1; 16]),
        phux_protocol::wire::frame::ExecState::Running,
    );
    assert!(!same_execution(
        &original,
        &original.clone().with_activation(Some(activation))
    ));
}

#[test]
fn reconnect_same_or_new_server_retires_bytes_and_restores_key_on_new_handle() {
    for server in [1, 2] {
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
        step(&mut rt);
        client.with_control(ControlPlane::connection_opened);
        step(&mut rt);
        assert!(rt.state.resources.entries.is_empty());
        assert!(rt.state.shown.is_none());
        outbound(&client);
        client.feed(hello(server)).unwrap();
        let moved = record(1, ResourceKind::AccessibilityTree).with_live(Some(id(9)));
        let reused = record(2, ResourceKind::AccessibilityTree).with_live(Some(id(1)));
        let records = [moved, reused];
        let attach_id = seed(&client, &records).expect("reconnect attach");
        install_arrangement(&client, &records, &doc(split(9, 1)));
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
        client.feed(FrameKind::AttachReady { attach_id }).unwrap();
        publish(
            &client,
            9,
            ResourceKind::AccessibilityTree,
            b"{\"fresh\":true}",
        );
        step(&mut rt);
        assert_eq!(rt.state.shown.as_ref().unwrap().terminal, id(9));
        assert!(
            matches!(&rt.state.resources.entries[&id(9)].body, Body::Tree(json) if json.contains("fresh") && !json.contains("old"))
        );
        assert!(matches!(
            rt.state.resources.entries[&id(1)].body,
            Body::Pending
        ));
    }
}

#[test]
fn epoch_retirement_reopens_by_key_not_reused_numeric_handle() {
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
    step(&mut rt);
    rt.update(|d, _| d.retire_documents(&client));
    assert!(rt.state.shown.is_none());
    assert!(rt.state.resources.entries.is_empty());
    assert_eq!(rt.state.resources.returning, Some(key(1)));
    // A replacement on the same address must not inherit old contents.
    let replacement = record(9, ResourceKind::AccessibilityTree).with_live(Some(id(1)));
    rt.state
        .resources
        .entries
        .insert(id(1), Document::new(replacement));
    rt.state.restore_document(&client);
    assert!(rt.state.wanted.is_none());
    assert!(matches!(
        rt.state.resources.entries[&id(1)].body,
        Body::Pending
    ));
}

#[test]
fn native_png_headless_renderer_smoke() {
    let (client, mut rt) = fixture(
        &[
            record(1, ResourceKind::PixelSurface),
            record(2, ResourceKind::AccessibilityTree),
        ],
        &doc(split(1, 2)),
    );
    publish(&client, 1, ResourceKind::PixelSurface, PNG);
    publish(&client, 2, ResourceKind::AccessibilityTree, br#"{"role":"document","title":"Native JSON inspector","children":[{"role":"button","name":"Read-only mirror"}]}"#);
    step(&mut rt);
    let Body::Surface(image) = &rt.state.resources.entries[&id(1)].body else {
        panic!("surface")
    };
    image.wait().unwrap();
    rt.update(|d, _| {
        d.finish_document_images();
        assert!(d.resources.entries[&id(1)].image_ready);
    });
    rt.frame(1.0);
    rt.frame(1.0);
    assert!(
        rt.instances
            .iter()
            .any(|i| i.params[1] == rune::render::KIND_IMAGE),
        "native image draw reaches Rune painter"
    );
    let gpu = rune::render::Gpu::headless();
    let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
    let target = rune::render::Offscreen::new(&gpu.device, 800, 600);
    renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
    renderer.prepare(
        &gpu.device,
        &gpu.queue,
        &mut rt.text.atlas,
        &rt.instances,
        (800, 600),
    );
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    renderer.draw(&mut enc, &target.view, [0.0; 4]);
    target.copy(&mut enc);
    renderer.submit(&gpu.queue, enc);
    let pixels = target.read(&gpu.device);
    let accessibility = rt.accessibility_json().to_string();
    assert!(
        accessibility.contains("Native JSON inspector"),
        "JSON is native accessible text beside the image"
    );
    assert!(accessibility.contains("Read-only mirror"));
    if let Some(path) = std::env::var_os("RUNE_RESOURCE_SNAP") {
        let path = std::path::PathBuf::from(path);
        rune::render::write_png(&path, 800, 600, &pixels).unwrap();
        std::fs::write(path.with_extension("json"), accessibility).unwrap();
        eprintln!("native resource frame: {}", path.display());
    }
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60)
            .count()
            > 500,
        "red half rendered"
    );
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[2] > 200 && p[0] < 60 && p[1] < 60)
            .count()
            > 500,
        "blue half rendered"
    );
}

#[test]
fn native_document_labels_distinguish_pending_empty_invalid_and_unavailable() {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::AccessibilityTree)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    assert!(
        rt.accessibility_json()
            .to_string()
            .contains("Waiting for the producer")
    );
    for (seq, bytes, label) in [
        (0, b"".as_slice(), "Empty document"),
        (1, b"not JSON", "Invalid JSON"),
    ] {
        rt.update(|d, _| {
            d.publish_document(&Arc::new(DocumentPublication {
                resource_id: id(1),
                kind: ResourceKind::AccessibilityTree,
                seq,
                bytes: bytes.to_vec(),
            }));
        });
        rt.frame(1.0);
        assert!(
            rt.accessibility_json().to_string().contains(label),
            "native label: {label}"
        );
    }
    remove(&client, 1, ResourceKind::AccessibilityTree);
    step(&mut rt);
    assert!(
        rt.accessibility_json()
            .to_string()
            .contains("Mirror unavailable")
    );
}
