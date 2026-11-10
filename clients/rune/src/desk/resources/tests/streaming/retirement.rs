//! Cache-wide admission survives arbitrary document/window turnover. The
//! decoder workers are real; a test-only waker barrier keeps them occupied
//! while new PNG jobs and identities arrive, without relying on decode timing.
use super::*;
use std::sync::{Condvar, Mutex};

struct Release(Arc<(Mutex<bool>, Condvar)>);

impl Drop for Release {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

fn busy_cache(rt: &mut Runtime<Desk>, base: &[u8]) -> (Release, mpsc::Receiver<()>) {
    let blocked = Arc::new((Mutex::new(false), Condvar::new()));
    let barrier = Arc::clone(&blocked);
    let (tx, wake) = mpsc::channel();
    rt.state.resources.images = Some(rune::ImageCache::new(Arc::new(move || {
        let _ = tx.send(());
        let guard = barrier.0.lock().unwrap();
        drop(barrier.1.wait_while(guard, |released| !*released).unwrap());
    })));
    let release = Release(blocked); // releases workers even if an assertion panics.
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(surface::MAX_DECODES);
    for n in 0..workers {
        // Warmers deliberately bypass the application's admission gate: they
        // occupy all actual workers before the application's first PNG job.
        rt.state
            .resources
            .images
            .as_ref()
            .unwrap()
            .load(stamped(base, 10_000 + n as u64));
    }
    for _ in 0..workers {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    (release, wake)
}

#[test]
fn decoded_after_scene_build_is_not_yet_render_eligible() {
    let base = png(0);
    let (client, mut rt, _, _) = live();
    let (release, wake) = busy_cache(&mut rt, &base);
    publish(&client, 1, ResourceKind::PixelSurface, &base);
    step(&mut rt); // Builds the placeholder while every worker is blocked.
    let image = rt.state.resources.entries[&id(1)]
        .surface
        .flight
        .as_ref()
        .unwrap()
        .image
        .clone();
    assert!(ready(&rt).is_none());
    drop(release);
    wake.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        image.size().is_some(),
        "the worker finished after the scene"
    );
    assert!(ready(&rt).is_none(), "decoding is not scene promotion");
    rt.update(|d, _| {
        d.finish_document_images();
    });
    rt.frame(1.0);
    rt.frame(1.0);
    assert_eq!(ready(&rt), Some(image.id()));
}

/// A new connection's attach, whose cut rides that attach's stream; returns it.
fn reattach(client: &Client, resource: ResourceRecord, bytes: &[u8]) -> StreamId {
    client.with_control(ControlPlane::connection_opened);
    outbound(client);
    client.feed(hello(1)).unwrap();
    let attach_id = seed(client, std::slice::from_ref(&resource)).unwrap();
    install_arrangement(client, &[resource], &doc(LayoutNode::Leaf(id(1))));
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), id(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(session())),
        ])
        .with_resources(vec![
            ResourceInfo::resource(id(1), ResourceKind::PixelSurface)
                .with_session(Some(SessionId::new(1))),
        ]);
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    let stream = StreamId::new(u64::from(attach_id) + 1).unwrap();
    publish_on(client, 1, stream, ResourceKind::PixelSurface, bytes);
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
    stream
}

/// Turn the identity over 64 times; returns the stream the last cut rides.
fn churn(mode: &str, client: &Arc<Client>, rt: &mut Runtime<Desk>, base: &[u8]) -> StreamId {
    let last = png(173);
    let mut stream = StreamId::new(1).unwrap();
    for n in 1..=64u8 {
        let bytes = stamped(if n == 64 { &last } else { base }, u64::from(n));
        let mut resource = record(n + 1, ResourceKind::PixelSurface).with_live(Some(id(1)));
        if mode == "activation" {
            resource = record(1, ResourceKind::PixelSurface).with_activation(Some(
                phux_protocol::wire::frame::ActivationInfo::new(
                    phux_protocol::ids::ActivationId::new(u64::from(n)),
                    phux_protocol::ids::ServerInstance::new([1; 16]),
                    phux_protocol::wire::frame::ExecState::Running,
                ),
            ));
        }
        if mode == "epoch" {
            stream = reattach(client, resource.clone(), &bytes);
        } else {
            replace_record(client, resource.clone());
            if mode == "renew" {
                let mut next = Desk::new(&Resolved::offline("cache renewal fixture"));
                rt.update(|d, _| {
                    d.carry_into(&mut next); // the real same-cache window handoff.
                    next.client = Some(Arc::clone(client));
                    *d = next;
                });
            }
            update(client, u64::from(n), &bytes);
        }
        observe(rt, client); // no waiting for any application decoder job.
        let document = &rt.state.resources.entries[&id(1)];
        assert_eq!(
            document.record, resource,
            "the new identity is actually installed"
        );
        assert!(ready(rt).is_none(), "retired pixels must never return");
        assert!(
            rt.state.resources.decodes.started <= surface::MAX_DECODES,
            "64 turnovers cannot create 64 queued cache jobs"
        );
    }
    assert_eq!(rt.state.resources.decodes.started, surface::MAX_DECODES);
    let document = &rt.state.resources.entries[&id(1)];
    assert!(document.surface.flight.is_none());
    assert!(document.surface.pending.is_some());
    assert!(matches!(document.body, Body::Decoding));
    stream
}

fn run_case(mode: &str, latest: &str) {
    let base = png(0);
    let (client, mut rt, _, _) = live();
    let (release, wake) = busy_cache(&mut rt, &base);
    publish(&client, 1, ResourceKind::PixelSurface, &base);
    observe(&mut rt, &client);
    let stream = churn(mode, &client, &mut rt, &base);
    let pending = rt.state.resources.entries[&id(1)]
        .surface
        .pending
        .as_ref()
        .unwrap()
        .as_ptr();
    rt.state.finish_document_images();
    assert_eq!(
        rt.state.resources.entries[&id(1)]
            .surface
            .pending
            .as_ref()
            .unwrap()
            .as_ptr(),
        pending,
        "a blocked admission retains its buffer instead of copying a PNG every poll"
    );
    if latest == "invalid" {
        update_on(&client, stream, 1000, b"malformed latest PNG");
        observe(&mut rt, &client);
        assert!(matches!(
            rt.state.resources.entries[&id(1)].body,
            Body::Invalid(_)
        ));
        assert!(rt.state.resources.entries[&id(1)].surface.pending.is_none());
    }
    drop(release);
    if latest == "newer" {
        // Free all old permits without polling the queued current cut. A
        // newer publication wins admission and must discard that queued cut.
        for _ in 0..surface::MAX_DECODES {
            wake.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        update_on(&client, stream, 1000, &stamped(&png(197), 1000));
        observe(&mut rt, &client);
        assert!(rt.state.resources.entries[&id(1)].surface.pending.is_none());
    } else if latest == "invalid" {
        for _ in 0..surface::MAX_DECODES {
            finish(&mut rt, &wake);
        }
        assert!(
            matches!(rt.state.resources.entries[&id(1)].body, Body::Invalid(_)),
            "retired completions cannot conceal the latest error"
        );
        assert_eq!(rt.state.resources.decodes.started, surface::MAX_DECODES);
        update_on(&client, stream, 1001, &stamped(&png(173), 1001));
        observe(&mut rt, &client);
    }
    while ready(&rt).is_none() {
        finish(&mut rt, &wake);
    }
    assert!(
        rt.state.resources.decodes.started <= surface::MAX_DECODES + 1,
        "only the current latest cut may follow the bounded retired jobs"
    );
    let (color, seq) = match latest {
        "newer" => (197, 1000),
        "invalid" => (173, 1001),
        _ => (173, 64),
    };
    let expected = rt
        .state
        .resources
        .images
        .as_ref()
        .unwrap()
        .load(stamped(&png(color), seq));
    assert_eq!(
        ready(&rt),
        Some(expected.id()),
        "the presented image is exactly the latest encoded cut"
    );
    step(&mut rt); // a renewed window also needs its normal focus/layout pass.
    let gpu = rune::render::Gpu::headless();
    let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
    rt.frame(1.0);
    rt.frame(1.0);
    let pixels = render(&mut rt, &gpu, &mut renderer);
    // The pane's focus glass tints these pixels slightly. Retired cuts have
    // red=0; the current cut's marker has red=173/197, also checked by exact
    // image identity above. Verify the real painter displays the new marker.
    assert!(
        marker(&pixels)[0] > 150,
        "GPU paints the latest identity's pixels, never an old completion"
    );
    eprintln!(
        "retirement acceptance: {mode}/{latest}, 64 turnovers, {} document decoder submissions (limit {} outstanding)",
        rt.state.resources.decodes.started,
        surface::MAX_DECODES
    );
}

#[test]
fn repeated_identity_retirement_cannot_reopen_the_same_cache_backlog() {
    for mode in ["epoch", "key", "activation", "renew"] {
        for latest in ["valid", "invalid", "newer"] {
            run_case(mode, latest);
        }
    }
}

#[test]
fn waiting_surface_gets_a_freed_slot_before_busy_surfaces_restart() {
    let base = png(0);
    let (_, mut rt, _, _) = live();
    let (release, wake) = busy_cache(&mut rt, &base);
    for n in 2..=5 {
        rt.state.resources.entries.insert(
            id(n),
            Document::new(record(n as u8, ResourceKind::PixelSurface)),
        );
    }
    let ids: Vec<_> = rt.state.resources.entries.keys().cloned().collect();
    let publication = |resource: &ResourceId, seq, stamp| {
        Arc::new(DocumentPublication {
            resource_id: resource.clone(),
            kind: ResourceKind::PixelSurface,
            seq,
            bytes: stamped(&base, stamp),
            content_type: None,
        })
    };
    for (n, resource) in ids[..4].iter().enumerate() {
        rt.state
            .publish_document(&publication(resource, 0, 100 + n as u64));
        rt.state
            .publish_document(&publication(resource, 1, 200 + n as u64));
    }
    let waiting = &ids[4]; // deliberately last in the stable map iteration.
    rt.state.publish_document(&publication(waiting, 0, 300));
    assert_eq!(rt.state.resources.entries[waiting].surface.started, 0);
    drop(release);
    for _ in 0..surface::MAX_DECODES {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    rt.state.finish_document_images();
    assert_eq!(
        rt.state.resources.entries[waiting].surface.started, 1,
        "a fifth surface cannot starve behind four continuously updating surfaces"
    );
}
