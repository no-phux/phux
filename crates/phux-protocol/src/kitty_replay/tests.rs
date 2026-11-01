use super::*;

mod residency;

#[test]
fn base64_encodes_padding_cases() {
    let mut out = Vec::new();
    write_base64(&mut out, b"f").expect("encode one");
    assert_eq!(out, b"Zg==");

    out.clear();
    write_base64(&mut out, b"fo").expect("encode two");
    assert_eq!(out, b"Zm8=");

    out.clear();
    write_base64(&mut out, b"foo").expect("encode three");
    assert_eq!(out, b"Zm9v");
}

fn terminal() -> GhosttyTerminal<'static, 'static> {
    let mut terminal = GhosttyTerminal::new(10, 5).expect("terminal");
    configure_terminal_for_kitty_graphics(&mut terminal).expect("kitty config");
    terminal.resize(10, 5, 8, 16).expect("cell geometry");
    terminal
}

fn replay(
    source: &GhosttyTerminal<'_, '_>,
    target: &mut GhosttyTerminal<'_, '_>,
    origin: (u16, u16),
) {
    // A fresh residency: the outer terminal is assumed to hold nothing.
    let out = replay_with(source, &mut KittyResidency::default(), origin);
    assert!(!out.is_empty());
    target.vt_write(&out);
}

fn replay_with(
    source: &GhosttyTerminal<'_, '_>,
    residency: &mut KittyResidency,
    origin: (u16, u16),
) -> Vec<u8> {
    let mut iter = PlacementIterator::new().expect("placement iterator");
    let mut out = Vec::new();
    emit_kitty_graphics_replay(source, &mut iter, residency, &mut out, origin, (10, 5))
        .expect("replay");
    out
}

type ObservedPlacement = (u32, u32, bool, [u32; 8], i32, Option<(i32, i32)>);

// Read the receiver's parsed placements, not the spelling of generated APCs.
fn placements(terminal: &GhosttyTerminal<'_, '_>) -> Vec<ObservedPlacement> {
    let graphics = terminal.kitty_graphics().expect("graphics");
    let mut iter = PlacementIterator::new().expect("iterator");
    let mut items = iter.update(&graphics).expect("placements");
    let mut result = Vec::new();
    while let Some(p) = items.next() {
        let image_id = p.image_id().expect("image id");
        let image = graphics.image(image_id).expect("image");
        let pos = p
            .viewport_pos(&image, terminal)
            .expect("position")
            .map(|p| (p.col, p.row));
        result.push((
            image_id,
            if p.is_external().expect("namespace") {
                p.placement_id().expect("placement id")
            } else {
                0
            },
            p.is_virtual().expect("virtual"),
            [
                p.columns().unwrap(),
                p.rows().unwrap(),
                p.source_x().unwrap(),
                p.source_y().unwrap(),
                p.source_width().unwrap(),
                p.source_height().unwrap(),
                p.x_offset().unwrap(),
                p.y_offset().unwrap(),
            ],
            p.z().expect("z"),
            pos,
        ));
    }
    result.sort();
    result
}

fn transmit(terminal: &mut GhosttyTerminal<'_, '_>, id: u32, width: u32, height: u32) {
    let mut command = format!("\x1b_Ga=t,f=32,s={width},v={height},i={id},q=2;").into_bytes();
    write_base64(&mut command, &vec![127; (width * height * 4) as usize]).expect("pixels");
    command.extend_from_slice(b"\x1b\\");
    terminal.vt_write(&command);
}

#[test]
fn replay_preserves_classic_geometry_identity_and_pixels() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    source.vt_write(
        b"\x1b[2;2H\x1b_Ga=p,i=7,p=9,c=2,r=2,x=1,y=2,w=5,h=4,X=2,Y=3,z=-17,C=1,q=2;\x1b\\",
    );
    let mut target = terminal();
    replay(&source, &mut target, (2, 1));
    let mut expected = placements(&source);
    expected[0].5 = Some((3, 2));
    assert_eq!(placements(&target), expected);
    let graphics = target.kitty_graphics().unwrap();
    assert_eq!(
        graphics.image(7).unwrap().data().unwrap().unwrap(),
        &[127; 256]
    );
    // Replaying the same placement does not accumulate anonymous copies.
    replay(&source, &mut target, (2, 1));
    assert_eq!(placements(&target), expected);
}

#[test]
fn replay_preserves_every_virtual_placement_and_natural_size() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    source.vt_write(b"\x1b_Ga=p,i=7,p=3,U=1,c=2,r=3,z=4,q=2;\x1b\\\x1b_Ga=p,i=7,p=4,U=1,c=4,r=2,x=1,w=4,z=-2,q=2;\x1b\\");
    transmit(&mut source, 8, 8, 8);
    source.vt_write(b"\x1b[4;2H\x1b_Ga=p,i=8,p=1,C=1,q=2;\x1b\\");
    let mut target = terminal();
    replay(&source, &mut target, (0, 0));
    assert_eq!(placements(&target), placements(&source));
    assert_eq!(placements(&target).len(), 3);
}

#[test]
fn replay_keeps_anonymous_and_external_namespaces_and_default_selection() {
    fn default_columns(terminal: &GhosttyTerminal<'_, '_>) -> u32 {
        let graphics = terminal.kitty_graphics().unwrap();
        let mut iter = PlacementIterator::new().unwrap();
        let mut items = iter.update(&graphics).unwrap();
        while let Some(p) = items.next() {
            if p.is_placeholder_default().unwrap() {
                return p.columns().unwrap();
            }
        }
        panic!("missing virtual default");
    }
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    // Internal ID 1 and external p=1 coexist; p=0 always creates a new placement.
    source.vt_write(b"\x1b_Ga=p,i=7,U=1,c=2,r=2,q=2;\x1b\\\x1b_Ga=p,i=7,U=1,c=3,r=2,q=2;\x1b\\\x1b_Ga=p,i=7,p=1,U=1,c=4,r=2,q=2;\x1b\\");
    let mut target = terminal();
    for _ in 0..2 {
        replay(&source, &mut target, (0, 0));
        assert_eq!(placements(&target), placements(&source));
        assert_eq!(placements(&target).len(), 3);
        assert_eq!(default_columns(&target), 4);
    }
    source.vt_write(b"\x1b_Ga=d,d=i,i=7,p=1,q=2;\x1b\\");
    for _ in 0..2 {
        replay(&source, &mut target, (0, 0));
        assert_eq!(placements(&target), placements(&source));
        assert_eq!(placements(&target).len(), 2);
        assert_eq!(default_columns(&target), 2);
    }
}

#[test]
fn snapshot_preserves_unplaced_payload_for_later_live_placement() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    assert!(placements(&source).is_empty());
    let mut target = terminal();
    let mut iter = PlacementIterator::new().unwrap();
    let mut out = Vec::new();
    assert!(emit_kitty_graphics_snapshot(&source, &mut iter, &mut out).unwrap());
    target.vt_write(&out);
    assert!(placements(&target).is_empty());
    assert_eq!(
        target
            .kitty_graphics()
            .unwrap()
            .image(7)
            .unwrap()
            .data()
            .unwrap()
            .unwrap(),
        &[127; 256]
    );
    let placement = b"\x1b_Ga=p,i=7,p=3,U=1,c=2,r=3,q=2;\x1b\\";
    source.vt_write(placement);
    target.vt_write(placement);
    assert_eq!(placements(&target), placements(&source));
    assert_eq!(placements(&target).len(), 1);
}

#[test]
fn viewport_replay_omits_unplaced_and_fully_clipped_payloads() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    let mut iter = PlacementIterator::new().unwrap();
    let mut residency = KittyResidency::default();
    let mut out = Vec::new();
    let mut emit = |source: &GhosttyTerminal<'_, '_>, out: &mut Vec<u8>| {
        emit_kitty_graphics_replay(source, &mut iter, &mut residency, out, (0, 0), (5, 3)).unwrap()
    };
    assert!(!emit(&source, &mut out));
    assert!(out.is_empty());

    source.vt_write(b"\x1b[4;9H\x1b_Ga=p,i=7,p=1,c=1,r=1,C=1,q=2;\x1b\\");
    assert!(!emit(&source, &mut out));
    assert!(out.is_empty());

    transmit(&mut source, 3, 8, 8);
    source.vt_write(b"\x1b[1;1H\x1b_Ga=p,i=3,p=2,c=1,r=1,C=1,q=2;\x1b\\");
    assert!(emit(&source, &mut out));
    let mut target = terminal();
    target.vt_write(&out);
    let graphics = target.kitty_graphics().unwrap();
    assert!(graphics.image(7).is_none());
    assert!(graphics.image(3).is_some());
    let items = placements(&target);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, 3);
    assert_eq!(items[0].5, Some((0, 0)));
}

#[test]
fn replay_crops_partially_scrolled_images_instead_of_dropping_them() {
    let mut source = terminal();
    transmit(&mut source, 7, 16, 64);
    source.vt_write(b"\x1b_Ga=p,i=7,p=1,c=2,r=4,C=1,q=2;\x1b\\\x1b[5;1H\n\n");
    let mut target = terminal();
    replay(&source, &mut target, (0, 0));
    let items = placements(&target);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].5, Some((0, 0)));
    assert_eq!(items[0].3, [2, 2, 0, 32, 0, 32, 0, 0]);
}

#[test]
fn replay_clips_bottom_right_without_rescaling_the_full_source() {
    let mut source = terminal();
    transmit(&mut source, 7, 32, 48);
    source.vt_write(b"\x1b[4;9H\x1b_Ga=p,i=7,p=1,c=4,r=3,C=1,q=2;\x1b\\");
    let mut target = terminal();
    replay(&source, &mut target, (0, 0));
    let items = placements(&target);
    assert_eq!(items[0].5, Some((8, 3)));
    assert_eq!(items[0].3, [2, 2, 0, 0, 16, 32, 0, 0]);
}

#[test]
fn replay_keeps_clipped_native_images_at_native_pixel_size() {
    let mut source = terminal();
    transmit(&mut source, 7, 16, 40);
    source.vt_write(b"\x1b[4;1H\x1b_Ga=p,i=7,p=1,C=1,q=2;\x1b\\");
    let mut target = terminal();
    replay(&source, &mut target, (0, 0));
    let items = placements(&target);
    assert_eq!(items[0].3, [0, 0, 0, 0, 0, 32, 0, 0]);
    let graphics = target.kitty_graphics().unwrap();
    let image = graphics.image(7).unwrap();
    let mut iter = PlacementIterator::new().unwrap();
    let mut placements = iter.update(&graphics).unwrap();
    assert_eq!(
        placements
            .next()
            .unwrap()
            .pixel_size(&image, &target)
            .unwrap()
            .height,
        32
    );
}

#[test]
fn enabling_graphics_does_not_enable_external_image_media() {
    let terminal = terminal();
    assert!(!terminal.is_kitty_image_from_file_allowed().unwrap());
    assert!(!terminal.is_kitty_image_from_shared_mem_allowed().unwrap());
    assert!(terminal.kitty_image_temp_file_dir().unwrap().is_none());
}

#[test]
fn png_formats_decode_to_rgba_and_roundtrip() {
    for (color, pixels, expected) in [
        (
            png::ColorType::Grayscale,
            vec![64, 128],
            vec![64, 64, 64, 255, 128, 128, 128, 255],
        ),
        (
            png::ColorType::GrayscaleAlpha,
            vec![64, 32, 128, 16],
            vec![64, 64, 64, 32, 128, 128, 128, 16],
        ),
        (
            png::ColorType::Indexed,
            vec![0, 1],
            vec![10, 20, 30, 255, 40, 50, 60, 42],
        ),
    ] {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            if color == png::ColorType::Indexed {
                encoder.set_palette(vec![10, 20, 30, 40, 50, 60]);
                encoder.set_trns(vec![255, 42]);
            }
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&pixels)
                .unwrap();
        }
        let mut command = b"\x1b_Ga=T,f=100,i=7,p=1,c=1,r=1,q=2;".to_vec();
        write_base64(&mut command, &png).unwrap();
        command.extend_from_slice(b"\x1b\\");
        let mut source = terminal();
        source.vt_write(&command);
        let mut target = terminal();
        replay(&source, &mut target, (0, 0));
        let graphics = target.kitty_graphics().unwrap();
        let image = graphics.image(7).expect("decoded PNG");
        assert_eq!(image.format().unwrap(), ImageFormat::Rgba);
        assert_eq!(image.data().unwrap().unwrap(), expected);
    }
}

#[test]
fn png_decoder_rejects_oversized_header_before_decoding() {
    // Retain valid compressed data but change the header to claim 1 GiB.
    // libghostty's capped decoder allocator rejects the RGBA output before
    // decoding into an image-sized global-heap scratch buffer.
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0; 4])
            .unwrap();
    }
    png[16..20].copy_from_slice(&16_384_u32.to_be_bytes());
    png[20..24].copy_from_slice(&16_384_u32.to_be_bytes());
    let mut crc = !0u32;
    for &byte in &png[12..29] {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    png[29..33].copy_from_slice(&(!crc).to_be_bytes());
    let mut command = b"\x1b_Ga=T,f=100,i=7,q=2;".to_vec();
    write_base64(&mut command, &png).unwrap();
    command.extend_from_slice(b"\x1b\\");
    let mut source = terminal();
    source.vt_write(&command);
    assert!(source.kitty_graphics().unwrap().image(7).is_none());
}

fn gradient(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i * 7 % 251).unwrap())
        .collect()
}

fn transmit_pixels(terminal: &mut GhosttyTerminal<'_, '_>, id: u32, format: u32, size: u32) {
    let bytes_per_pixel = format as usize / 8;
    let pixels = gradient(size as usize * size as usize * bytes_per_pixel);
    let mut command = format!("\x1b_Ga=t,f={format},s={size},v={size},i={id},q=2;").into_bytes();
    write_base64(&mut command, &pixels).expect("pixels");
    command.extend_from_slice(b"\x1b\\");
    terminal.vt_write(&command);
}

fn contains(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

fn pixels(terminal: &GhosttyTerminal<'_, '_>, id: u32) -> Option<Vec<u8>> {
    let graphics = terminal.kitty_graphics().unwrap();
    Some(graphics.image(id)?.data().unwrap()?.to_vec())
}
