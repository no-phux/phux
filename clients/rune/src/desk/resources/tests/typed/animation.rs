//! A typed document inherits Rune's animation without another publication.
use super::*;

fn gif() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
    encoder
        .set_repeat(image::codecs::gif::Repeat::Infinite)
        .unwrap();
    for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
        encoder
            .encode_frame(image::Frame::from_parts(
                image::RgbaImage::from_pixel(4, 4, image::Rgba(color)),
                0,
                0,
                image::Delay::from_numer_denom_ms(200, 1),
            ))
            .unwrap();
    }
    drop(encoder);
    bytes
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

fn colored(pixels: &[u8], channel: usize) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[channel] > 220 && p[2 - channel] < 40 && p[1] < 40)
        .count()
}

#[test]
fn typed_gif_advances_gpu_pixels_without_new_document_output() {
    let gpu = rune::render::Gpu::headless();
    for phone in [false, true] {
        let (_, mut rt) = fixture(
            &[record(1, ResourceKind::Document)],
            &doc(LayoutNode::Leaf(id(1))),
        );
        rt.update(|d, _| d.phone = phone.then(Default::default));
        step(&mut rt); // settle the phone camera/layout before the image's clock starts
        let (decoded, wake) = std::sync::mpsc::channel();
        rt.update(|d, _| {
            d.resources.images = Some(rune::ImageCache::new(Arc::new(move || {
                let _ = decoded.send(());
            })));
            d.publish_document(&Arc::new(publication(0, "image/gif", gif())));
        });
        let Body::Surface(image) = &rt.state.resources.entries[&id(1)].body else {
            panic!("typed image")
        };
        image.wait().unwrap();
        wake.recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        rt.update(|d, _| {
            d.finish_document_images();
        });
        rt.frame(0.0);
        rt.frame(0.0); // layout feedback, but keep the first animation frame
        let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
        assert!(
            colored(&render(&mut rt, &gpu, &mut renderer), 0) > 1000,
            "first frame, phone={phone}"
        );
        rt.frame(0.25);
        assert!(
            colored(&render(&mut rt, &gpu, &mut renderer), 2) > 1000,
            "second frame, phone={phone}"
        );
        assert_eq!(rt.state.resources.entries[&id(1)].seq, Some(0));
        assert_eq!(rt.state.resources.entries[&id(1)].surface.started, 1);
    }
}
