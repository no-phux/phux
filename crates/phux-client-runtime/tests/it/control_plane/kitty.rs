//! A natural-size kitty image must use the painter's cell pitch on the first
//! published frame, including the frame a resync publishes. The replica is
//! created at 8×16; dividing by that pitch is the detach/reattach stretch.

use super::*;

#[test]
fn local_cell_pixels_size_the_first_kitty_frame_and_its_resync() {
    for height in [270_u32, 272] {
        let mut plane = browse();
        assert_eq!(
            plane.resize_terminal_with_cell(&terminal(), 80, 24, Some((8, 17))),
            phux_client_runtime::control::TerminalResizeOutcome::NotReady,
            "the settled resize must not be what installs the pitch"
        );
        plane.set_local_cell_pixels(&terminal(), 8, 17);
        assert!(
            resize_frames(&mut plane).is_empty(),
            "local cell pixels send no server message"
        );

        let frame = publish_natural_image(&mut plane, 1, height);
        assert_eq!(frame.generation, 1, "the first publish is the snapshot");
        assert_natural_rows(&frame, height);

        let replaced = publish_natural_image(&mut plane, 2, height);
        assert_eq!(replaced.generation, 2, "a resync publishes one replacement");
        assert_eq!(replaced.bootstrap_id, 2);
        assert_natural_rows(&replaced, height);
        assert!(
            resize_frames(&mut plane).is_empty(),
            "neither publish queues a resize"
        );
    }
}

fn browse() -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    plane.take_outbound();
    plane
}

fn publish_natural_image(
    plane: &mut ControlPlane,
    generation: u64,
    height: u32,
) -> std::sync::Arc<phux_client_runtime::publication::GridFrame> {
    let previous = plane.publication().generation(&terminal()).unwrap_or(0);
    let stream_id = StreamId::new(generation).unwrap();
    let bootstrap_id = BootstrapId::new(generation).unwrap();
    let id = terminal();
    plane
        .feed(FrameKind::BootstrapBegin {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        })
        .unwrap();
    // One direct-RGB placement is larger than the negotiated chunk cap. The
    // VT stream is the concatenation, so the APC itself stays one command.
    for (chunk_seq, payload) in natural_kitty(480, height).chunks(200 * 1024).enumerate() {
        plane
            .feed(FrameKind::BootstrapChunk {
                terminal_id: id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: u32::try_from(chunk_seq).unwrap(),
                payload: payload.to_vec().into(),
            })
            .unwrap();
    }
    plane
        .feed(FrameKind::BootstrapReady {
            terminal_id: id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        })
        .unwrap();
    let frame = plane.publication().acquire(&terminal()).expect("published");
    assert_eq!(
        frame.generation,
        previous + 1,
        "no corrective republish after the snapshot"
    );
    frame
}

fn assert_natural_rows(frame: &phux_client_runtime::publication::GridFrame, height: u32) {
    assert_eq!(frame.images.len(), 1, "the natural image is on this frame");
    let placement = &frame.images[0];
    assert_eq!(
        (placement.image.width, placement.image.height),
        (480, height)
    );
    let rows = f64::from(height) / 17.0;
    assert_eq!(
        (placement.cols, placement.rows),
        (480.0 / 8.0, rows),
        "272px at 17px is 16 rows; 270px must not grow to {height}/16"
    );
}

/// Direct RGB placement with pixel size and no `c` or `r`.
fn natural_kitty(width: u32, height: u32) -> Vec<u8> {
    let mut rgb = vec![0xff_u8; usize::try_from(width * height * 3).unwrap()];
    let (pixels, _) = rgb.as_chunks_mut::<3>();
    for pixel in pixels {
        pixel[1] = 0;
        pixel[2] = 0;
    }
    let header = format!("\x1b_Ga=T,f=24,s={width},v={height},i=1,q=2,C=1;");
    assert!(!header.contains(",c=") && !header.contains(",r="));
    let mut apc = header.into_bytes();
    apc.extend_from_slice(base64(&rgb).as_bytes());
    apc.extend_from_slice(b"\x1b\\");
    apc
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    let (chunks, rest) = bytes.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.extend([
            TABLE[((n >> 18) & 0x3f) as usize],
            TABLE[((n >> 12) & 0x3f) as usize],
            TABLE[((n >> 6) & 0x3f) as usize],
            TABLE[(n & 0x3f) as usize],
        ]);
    }
    if !rest.is_empty() {
        let b0 = rest[0];
        let b1 = rest.get(1).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8);
        let pad = if rest.len() == 1 {
            b'='
        } else {
            TABLE[((n >> 6) & 0x3f) as usize]
        };
        out.extend([
            TABLE[((n >> 18) & 0x3f) as usize],
            TABLE[((n >> 12) & 0x3f) as usize],
            pad,
            b'=',
        ]);
    }
    String::from_utf8(out).expect("base64 is ascii")
}

fn resize_frames(plane: &mut ControlPlane) -> Vec<FrameKind> {
    plane
        .take_outbound()
        .iter()
        .map(|frame| decode(frame))
        .filter(|frame| matches!(frame, FrameKind::ResizeTerminal { .. }))
        .collect()
}
