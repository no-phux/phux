//! Official 1.0 image records stay distinct from history rows and FINISH even
//! when the freestanding engine does not render Kitty graphics.

use phux_vt_web::{HistoryProgressKind, NativeCodecError, NativeDecoder, Vt};
use wasm_bindgen_test::wasm_bindgen_test;

fn fixture() -> (Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
    let bytes: Vec<u8> = include_str!("fixtures/upstream-kitty-animation-v1.hex")
        .lines()
        .flat_map(|line| line.split('#').next().expect("line").split_whitespace())
        .map(|hex| u8::from_str_radix(hex, 16).expect("fixture hex"))
        .collect();
    assert_eq!(&bytes[..10], b"GHOSTSNP\x01\0");
    let mut offset = 10;
    let mut ready_end = None;
    let mut images = Vec::new();
    let mut finish_start = None;
    while offset < bytes.len() {
        let tag = u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("tag"));
        let len =
            u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().expect("len")) as usize;
        let end = offset + 10 + len;
        match tag {
            5 => ready_end = Some(end),
            9 => images.push(bytes[offset..end].to_vec()),
            4 => finish_start = Some(offset),
            _ => {}
        }
        offset = end;
    }
    assert_eq!(images.len(), 3, "three image/animation frames");
    (
        bytes[..ready_end.expect("READY")].to_vec(),
        images,
        bytes[finish_start.expect("HISTORY + FINISH")..].to_vec(),
    )
}

fn feed(decoder: &mut NativeDecoder, bytes: &[u8], cut: usize) {
    for fragment in bytes.chunks(cut) {
        decoder.feed(fragment).expect("stage fragment");
    }
}

#[wasm_bindgen_test]
async fn kitty_image_units_report_progress_not_finish() {
    let (ready, images, finish) = fixture();
    let vt = Vt::load().await.expect("load engine");
    for cut in [1, 7, usize::MAX] {
        let mut decoder = vt.native_decoder(1024 * 1024).expect("decoder");
        feed(&mut decoder, &ready, cut);
        let terminal = decoder.ready().expect("Kitty-bearing READY");
        assert_eq!((terminal.grid().cols, terminal.grid().rows), (4, 2));
        terminal.write(b"live");
        for image in &images {
            feed(&mut decoder, image, cut);
            let progress = decoder
                .next_checked(0)
                .expect("authenticate image pixels")
                .expect("image is not FINISH");
            assert_eq!(progress.kind, HistoryProgressKind::Image);
            assert_eq!(progress.authenticated_rows, 0);
            assert_eq!(progress.rows, 0);
            assert_eq!(decoder.staged_capacity(), 0);
            assert_eq!(
                decoder.terminal().expect("live terminal").rows_text()[0],
                "live"
            );
        }
        feed(&mut decoder, &finish, cut);
        assert_eq!(decoder.next_checked(0), Ok(None), "authenticated FINISH");
    }
}

#[wasm_bindgen_test]
async fn corrupt_or_row_bearing_images_fail_closed() {
    let (ready, images, _) = fixture();
    let vt = Vt::load().await.expect("load engine");
    for declared_rows in [0, 1] {
        let mut decoder = vt.native_decoder(1024 * 1024).expect("decoder");
        feed(&mut decoder, &ready, 7);
        decoder.ready().expect("READY");
        let before = decoder.terminal().expect("terminal").screen_rows();
        let mut image = images[0].clone();
        if declared_rows == 0 {
            *image.last_mut().expect("pixels") ^= 0x40;
        }
        feed(&mut decoder, &image, 1);
        let error = decoder
            .next_checked(declared_rows)
            .expect_err("reject image");
        if declared_rows == 0 {
            assert!(
                matches!(error, NativeCodecError::Status { .. }),
                "bad CRC: {error}"
            );
        } else {
            assert_eq!(error, NativeCodecError::InvalidUnit, "images have no rows");
        }
        assert_eq!(
            decoder.terminal().expect("terminal survives").screen_rows(),
            before
        );
        assert_eq!(
            decoder.feed(&images[1]),
            Err(NativeCodecError::InvalidState)
        );
    }
}
