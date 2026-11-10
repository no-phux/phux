//! Real PNG workers and GPU readback while a wire producer keeps publishing.
use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;

mod retirement;

fn crc(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn png(marker: u32) -> Vec<u8> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    // Nontrivial screenshot dimensions, real encoding and mip generation.
    let (w, h) = (1024, 576);
    let pixels: Vec<_> = (0..w * h)
        .flat_map(|i| {
            if i / w < h / 8 {
                [marker as u8, 255, (marker / 256) as u8, 255]
            } else if i % w < w / 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            }
        })
        .collect();
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target-rune")
        .join(format!(
            "stream-fixture-{}-{}.png",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    rune::render::write_png(&path, w, h, &pixels).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    bytes
}

fn stamped(base: &[u8], seq: u64) -> Vec<u8> {
    // A valid, unique PNG tEXt chunk defeats content-cache deduplication.
    // Every update really would queue a fresh decoder job in the old path.
    let mut chunk = b"tEXtframe\0".to_vec();
    chunk.extend_from_slice(seq.to_string().as_bytes());
    let mut bytes = base[..base.len() - 12].to_vec();
    bytes.extend_from_slice(&((chunk.len() - 4) as u32).to_be_bytes());
    bytes.extend_from_slice(&chunk);
    bytes.extend_from_slice(&crc(&chunk).to_be_bytes());
    bytes.extend_from_slice(&base[base.len() - 12..]);
    bytes
}

fn live() -> (
    Arc<Client>,
    Runtime<Desk>,
    mpsc::Receiver<()>,
    Arc<AtomicUsize>,
) {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::PixelSurface)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    let (tx, rx) = mpsc::channel();
    let completed = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&completed);
    rt.state.resources.images = Some(rune::ImageCache::new(Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        let _ = tx.send(());
    })));
    (client, rt, rx, completed)
}

fn update(client: &Client, seq: u64, bytes: &[u8]) {
    update_on(client, StreamId::new(1).unwrap(), seq, bytes);
}

fn update_on(client: &Client, stream_id: StreamId, seq: u64, bytes: &[u8]) {
    client
        .feed(FrameKind::ResourceOutput {
            terminal_id: id(1),
            stream_id,
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: bytes.to_vec().into(),
        })
        .unwrap();
}

fn ready(rt: &Runtime<Desk>) -> Option<rune::ImageId> {
    let document = &rt.state.resources.entries[&id(1)];
    match &document.body {
        // A first image is exposed while decoding. Its worker may finish
        // after step() built the scene; wait for application promotion so
        // the scene, not just the live handle, has its uploadable pixels.
        Body::Surface(image)
            if image.size().is_some()
                && image.error().is_none()
                && document
                    .surface
                    .flight
                    .as_ref()
                    .is_none_or(|job| job.image.id() != image.id()) =>
        {
            Some(image.id())
        }
        _ => None,
    }
}

fn finish(rt: &mut Runtime<Desk>, wake: &mpsc::Receiver<()>) {
    wake.recv_timeout(Duration::from_secs(10))
        .expect("real decoder completion, not a timing sleep");
    rt.update(|d, _| {
        d.finish_document_images();
    });
    rt.frame(1.0);
    rt.frame(1.0);
}

fn render(
    rt: &mut Runtime<Desk>,
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
) -> Vec<u8> {
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
    target.read(&gpu.device)
}

fn marker(pixels: &[u8]) -> [u8; 4] {
    let mut colors = std::collections::HashMap::new();
    for pixel in pixels.as_chunks::<4>().0.iter().filter(|p| p[1] > 240) {
        *colors.entry(*pixel).or_insert(0usize) += 1;
    }
    let (color, count) = colors.into_iter().max_by_key(|(_, count)| *count).unwrap();
    assert!(count > 1000, "producer marker was actually GPU-rendered");
    color
}

#[test]
fn sustained_unique_png_wire_bursts_render_first_and_subsequent_complete_frames() {
    let base = png(0);
    let (client, mut rt, wake, completed) = live();
    publish(&client, 1, ResourceKind::PixelSurface, &stamped(&base, 0));
    step(&mut rt);
    let first = rt.state.resources.entries[&id(1)]
        .surface
        .flight
        .as_ref()
        .unwrap()
        .image
        .id();
    let (batch_tx, batch_rx) = mpsc::sync_channel(1);
    let (next_tx, next_rx) = mpsc::sync_channel(1);
    let producer_client = Arc::clone(&client);
    let producer = std::thread::spawn(move || {
        for batch in 1..=512 {
            let base = png(batch as u32);
            for seq in ((batch - 1) * 64 + 1)..=(batch * 64) {
                update(&producer_client, seq, &stamped(&base, seq));
            }
            if batch_tx.send(batch * 64).is_err()
                || next_rx.recv_timeout(Duration::from_secs(10)).is_err()
            {
                break;
            }
        }
    });
    let gpu = rune::render::Gpu::headless();
    let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
    let mut rendered = Vec::new();
    let mut markers = Vec::new();
    let mut published = 0;
    for _ in 0..512 {
        published = batch_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // Continue publishing before and during rendering; there is no
        // stop-the-producer / wait-for-the-last-static-frame acceptance step.
        step(&mut rt);
        next_tx.send(()).unwrap();
        if let Some(image) = ready(&rt)
            && rendered.last() != Some(&image)
        {
            let pixels = render(&mut rt, &gpu, &mut renderer);
            markers.push(marker(&pixels));
            for channel in [0, 2] {
                assert!(
                    pixels
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .filter(|p| p[channel] > 220 && p[1] < 40)
                        .count()
                        > 1000
                );
            }
            rendered.push(image);
        }
        let d = &rt.state.resources.entries[&id(1)];
        assert!(
            d.surface.started <= completed.load(Ordering::SeqCst) + 1,
            "at most one queued/running decode"
        );
        assert!(
            d.surface.started < published as usize,
            "bursts coalesce rather than queue every PNG"
        );
        if rendered.len() >= 3 {
            break;
        }
        // Observe the worker through its actual completion signal, while the
        // bounded producer is still publishing the next burst.
        if ready(&rt).is_none() {
            wake.recv_timeout(Duration::from_secs(10)).unwrap();
        }
    }
    drop(next_tx);
    drop(batch_rx);
    producer.join().unwrap();
    assert_eq!(
        rendered.first(),
        Some(&first),
        "the first in-flight image is not abandoned"
    );
    assert!(
        rendered.len() >= 3,
        "sustained input must render subsequent complete images too"
    );
    assert!(published >= 192);
    assert!(
        markers.iter().any(|color| *color != markers[0]),
        "GPU pixels, not just image handles, advance with the ongoing producer"
    );
    // Drain to the latest cut after proving live progress, then verify the
    // cache really ran only the bounded set of submitted decoder jobs.
    step(&mut rt);
    while rt.state.resources.entries[&id(1)].surface.flight.is_some() {
        finish(&mut rt, &wake);
    }
    assert_eq!(
        rt.state.resources.entries[&id(1)].surface.started,
        completed.load(Ordering::SeqCst)
    );
    assert!(rt.state.resources.entries[&id(1)].surface.pending.is_none());
    eprintln!(
        "live PNG acceptance: {published}+ unique wire updates, {} distinct GPU-rendered images, {} real decoder jobs",
        rendered.len(),
        completed.load(Ordering::SeqCst)
    );
}

#[test]
fn invalid_or_empty_new_png_clears_old_pixels_and_cannot_be_overwritten_by_an_old_decode() {
    let (client, mut rt, wake, _) = live();
    publish(&client, 1, ResourceKind::PixelSurface, PNG);
    step(&mut rt);
    finish(&mut rt, &wake);
    let old = ready(&rt).unwrap();
    update(&client, 1, &stamped(PNG, 1));
    step(&mut rt);
    assert_eq!(
        ready(&rt),
        Some(old),
        "last good pixels stay while a valid next frame decodes"
    );
    update(&client, 2, b"not PNG");
    step(&mut rt);
    assert!(
        matches!(&rt.state.resources.entries[&id(1)].body, Body::Invalid(error) if error.contains("PNG"))
    );
    finish(&mut rt, &wake);
    assert!(
        ready(&rt).is_none(),
        "obsolete completion never resurrects stale pixels"
    );
    update(&client, 3, &PNG[..24]); // signature is valid; actual decoder must reject it.
    step(&mut rt);
    finish(&mut rt, &wake);
    assert!(
        matches!(&rt.state.resources.entries[&id(1)].body, Body::Invalid(error) if error.contains("PNG"))
    );
    assert!(
        !rt.instances
            .iter()
            .any(|i| i.params[1] == rune::render::KIND_IMAGE)
    );
    update(&client, 4, PNG);
    step(&mut rt);
    finish(&mut rt, &wake);
    assert!(ready(&rt).is_some(), "a later valid cut recovers");
    update(&client, 5, b"");
    step(&mut rt);
    assert!(matches!(
        rt.state.resources.entries[&id(1)].body,
        Body::Empty
    ));
}

#[test]
fn real_png_decoder_keeps_dimension_limits() {
    for (width, height, depth) in [(8193u32, 2u32, 8u8), (8192, 8192, 16)] {
        let (client, mut rt, wake, _) = live();
        let mut bytes = PNG.to_vec();
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        bytes[24] = depth;
        let checksum = crc(&bytes[12..29]);
        bytes[29..33].copy_from_slice(&checksum.to_be_bytes());
        publish(&client, 1, ResourceKind::PixelSurface, &bytes);
        step(&mut rt);
        finish(&mut rt, &wake);
        assert!(
            matches!(&rt.state.resources.entries[&id(1)].body, Body::Invalid(error) if error.to_lowercase().contains("limit"))
        );
        assert!(ready(&rt).is_none());
    }
}

fn observe(rt: &mut Runtime<Desk>, client: &Client) {
    let observation = client.take_activity();
    rt.update(|d, tx| {
        d.observe(client, observation, tx);
    });
}

#[test]
fn malformed_latest_png_queued_behind_a_valid_decode_replaces_last_good_pixels_with_error() {
    let (client, mut rt, wake, _) = live();
    publish(&client, 1, ResourceKind::PixelSurface, PNG);
    step(&mut rt);
    finish(&mut rt, &wake);
    update(&client, 1, &stamped(PNG, 1));
    observe(&mut rt, &client);
    update(&client, 2, &PNG[..24]);
    observe(&mut rt, &client);
    assert_eq!(
        rt.state.resources.entries[&id(1)]
            .surface
            .pending
            .as_deref(),
        Some(&PNG[..24])
    );
    finish(&mut rt, &wake); // valid flight completes; malformed latest starts.
    assert!(ready(&rt).is_some());
    finish(&mut rt, &wake); // real decoder reports malformed PNG, not stale success.
    assert!(
        matches!(&rt.state.resources.entries[&id(1)].body, Body::Invalid(error) if error.contains("PNG"))
    );
    assert!(
        !rt.instances
            .iter()
            .any(|i| i.params[1] == rune::render::KIND_IMAGE)
    );
}

fn replace_record(client: &Client, record: ResourceRecord) {
    let request_id = client.refresh_organization().unwrap();
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(2), 0)
        .with_sessions(vec![
            SessionRecord::new(session(), "work", Revision::new(1))
                .with_arrangement(Some(arrangement()))
                .with_live(Some(SessionId::new(1))),
        ])
        .with_resources(vec![record]);
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .unwrap();
}

#[test]
fn resource_identity_teardown_drops_latest_png_and_ignores_old_worker_completion() {
    for change in ["key", "activation", "epoch", "removed"] {
        let (client, mut rt, wake, completed) = live();
        publish(&client, 1, ResourceKind::PixelSurface, PNG);
        observe(&mut rt, &client);
        update(&client, 1, &stamped(PNG, 1));
        observe(&mut rt, &client);
        assert!(rt.state.resources.entries[&id(1)].surface.pending.is_some());
        match change {
            "key" => replace_record(
                &client,
                record(2, ResourceKind::PixelSurface).with_live(Some(id(1))),
            ),
            "activation" => replace_record(
                &client,
                record(1, ResourceKind::PixelSurface).with_activation(Some(
                    phux_protocol::wire::frame::ActivationInfo::new(
                        phux_protocol::ids::ActivationId::new(1),
                        phux_protocol::ids::ServerInstance::new([2; 16]),
                        phux_protocol::wire::frame::ExecState::Running,
                    ),
                )),
            ),
            "epoch" => client.with_control(ControlPlane::connection_opened),
            _ => remove(&client, 1, ResourceKind::PixelSurface),
        }
        observe(&mut rt, &client);
        finish(&mut rt, &wake); // the dropped document's real cache job completes.
        assert_eq!(
            completed.load(Ordering::SeqCst),
            1,
            "pending PNG was never submitted after teardown"
        );
        if let Some(document) = rt.state.resources.entries.get(&id(1)) {
            assert!(matches!(document.body, Body::Pending));
            assert!(document.surface.flight.is_none());
            assert!(document.surface.pending.is_none());
            assert_eq!(document.surface.started, 0);
        }
    }
}
