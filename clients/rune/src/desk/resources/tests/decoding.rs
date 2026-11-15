//! Real image decoder acceptance, independent of workspace lifecycle.
use super::*;

#[test]
fn png_uses_real_decoder_and_reports_truncated_png() {
    let (decoded, wake) = std::sync::mpsc::channel();
    let cache = rune::ImageCache::new(Arc::new(move || {
        let _ = decoded.send(());
    }));
    let mut d = Document::new(record(1, ResourceKind::PixelSurface));
    let mut decodes = surface::Decodes::default();
    let mut publication = DocumentPublication {
        resource_id: id(1),
        kind: ResourceKind::PixelSurface,
        seq: 0,
        bytes: PNG.to_vec(),
        content_type: None,
    };
    assert!(d.publish(&publication, Some(&cache), &mut decodes));
    let Body::Surface(image) = &d.body else {
        panic!("image")
    };
    assert_eq!(image.wait(), Ok([4, 2]));
    wake.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert!(
        !d.publish(&publication, Some(&cache), &mut decodes),
        "stale sequence ignored"
    );
    publication.seq = 1;
    publication.bytes = PNG[..24].to_vec();
    assert!(d.publish(&publication, Some(&cache), &mut decodes));
    // Retire the completed first decode, then decode the queued next cut.
    d.finish_surface(Some(&cache), &mut decodes);
    assert!(d.surface.flight.as_ref().unwrap().image.wait().is_err());
    wake.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    d.finish_surface(Some(&cache), &mut decodes);
    assert!(matches!(&d.body, Body::Invalid(error) if error.contains("PNG")));
}

#[test]
fn a_jpeg_frame_uses_the_real_decoder() {
    const JPEG: &[u8] =
        include_bytes!("../../../../../../crates/phux-core/testdata/baseline-5x3.jpg");
    let cache = rune::ImageCache::new(Arc::new(|| {}));
    let mut d = Document::new(record(1, ResourceKind::PixelSurface));
    let publication = DocumentPublication {
        resource_id: id(1),
        kind: ResourceKind::PixelSurface,
        seq: 0,
        bytes: JPEG.to_vec(),
        content_type: None,
    };
    assert!(d.publish(&publication, Some(&cache), &mut surface::Decodes::default()));
    let Body::Surface(image) = &d.body else {
        panic!("image")
    };
    assert_eq!(image.wait(), Ok([5, 3]));
}
