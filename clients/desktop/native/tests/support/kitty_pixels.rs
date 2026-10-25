//! Kitty APC bytes through the real replica and native GPU painter.

use super::*;

const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];

pub(super) fn kitty_graphics_paints_rgb_png_and_unicode_placeholders() {
    let (client, view) = publish(b"\x1b_Ga=T,f=24,s=1,v=1,c=2,r=2,i=42,C=1;/wAA\x1b\\");
    let shot = paint(client, view, settings(false));
    assert_mostly(&cell_pixels(&shot, 0, 0), RED, "RGB image top left");
    assert_mostly(&cell_pixels(&shot, 1, 1), RED, "RGB image bottom right");
    assert_blank(&cell_pixels(&shot, 0, 2), "outside image");

    let (client, view) = publish(
        b"\x1b_Ga=T,f=100,c=2,r=2,i=43,C=1;\
          iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAA\
          DUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\x1b\\",
    );
    let shot = paint(client, view, settings(false));
    assert_mostly(&cell_pixels(&shot, 1, 1), RED, "decoded PNG image");

    let (client, view) = publish(
        concat!(
            "\x1b_Ga=T,f=24,s=1,v=2,c=2,r=2,i=42,U=1,C=1;AAD/AAD/\x1b\\",
            "\x1b[38;5;42m\u{10eeee}\u{305}\u{305}\u{10eeee}",
            "\x1b[2;1H\u{10eeee}\u{30d}\u{305}\u{10eeee}\x1b[m",
        )
        .as_bytes(),
    );
    let shot = paint(client, view, settings(false));
    assert_mostly(&cell_pixels(&shot, 0, 0), BLUE, "placeholder first cell");
    assert_mostly(
        &cell_pixels(&shot, 1, 1),
        BLUE,
        "inherited placeholder column",
    );
    assert_blank(&cell_pixels(&shot, 0, 2), "outside placeholders");
}

pub(super) fn kitty_graphics_preserves_crop_alpha_and_z_order() {
    let (client, view) = publish(
        concat!(
            "\x1b_Ga=T,f=24,s=2,v=2,c=2,r=1,i=1,x=1,y=0,w=1,h=1,C=1;/wAAAP8AAAD///8A\x1b\\",
            "\x1b[2;1H\x1b_Ga=T,f=24,s=1,v=1,c=4,r=1,i=2,z=-1,C=1;AAD/\x1b\\",
            "M\x1b[41m \x1b[m",
            "\x1b[3;1H\x1b_Ga=T,f=24,s=1,v=1,c=4,r=1,i=3,z=-1073741825,C=1;AAD/\x1b\\",
            "M\x1b[41m \x1b[m",
            "\x1b[4;1H\x1b_Ga=T,f=24,s=1,v=1,c=2,r=1,i=4,z=0,C=1;AAD/\x1b\\",
            "\x1b_Ga=T,f=32,s=1,v=1,c=2,r=1,i=5,z=1,C=1;/wAAgA==\x1b\\",
        )
        .as_bytes(),
    );
    let shot = paint(client, view, settings(false));
    assert_mostly(
        &cell_pixels(&shot, 0, 1),
        [0, 255, 0],
        "source crop, not full-image scaling",
    );
    assert_mostly(
        &cell_pixels(&shot, 1, 1),
        BLUE,
        "negative image above cell background",
    );
    assert_mostly(
        &cell_pixels(&shot, 2, 1),
        AMBER,
        "lowest image below explicit cell background",
    );
    assert_mostly(
        &cell_pixels(&shot, 2, 2),
        BLUE,
        "lowest image over default background",
    );
    assert_mostly(
        &cell_pixels(&shot, 3, 1),
        [128, 0, 127],
        "straight alpha and image z order",
    );
    let foreground = cell_pixels(&shot, 1, 0)
        .into_iter()
        .filter(|p| near(*p, FOREGROUND))
        .count();
    assert!(foreground > 3, "negative image must leave text on top");
}

pub(super) fn kitty_graphics_replaces_and_deletes_cached_pixels() {
    let (client, view) = publish(b"\x1b_Ga=T,f=24,s=1,v=1,c=2,r=2,i=42,C=1;/wAA\x1b\\");
    let mut cx = gpu();
    let in_view = Arc::clone(&client);
    let window = cx
        .open_window(size(px(480.), px(200.)), move |window, cx| {
            cx.new(|cx| Terminal::new(in_view, view, terminal(), settings(false), window, cx))
        })
        .expect("offscreen window");
    let draw = |cx: &mut HeadlessAppContext| {
        window.update(cx, |_, _, cx| cx.notify()).expect("notify");
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .expect("draw");
        Shot {
            painted: window
                .update(cx, |terminal, _, _| terminal.painted())
                .expect("painted"),
            image: cx.capture_screenshot(window.into()).expect("pixels"),
        }
    };
    assert_mostly(&cell_pixels(&draw(&mut cx), 0, 0), RED, "initial image");
    for (seq, payload, expected) in [
        (
            1,
            b"\x1b[H\x1b_Ga=T,f=24,s=1,v=1,c=2,r=2,i=42,C=1;AAD/\x1b\\".as_slice(),
            BLUE,
        ),
        (2, b"\x1b_Ga=d,d=I,i=42\x1b\\".as_slice(), BACKGROUND),
    ] {
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: payload.to_vec().into(),
            })
            .expect("live image change");
        let shot = draw(&mut cx);
        assert!(shot.painted.error.is_none(), "{:?}", shot.painted.error);
        assert_mostly(
            &cell_pixels(&shot, 0, 0),
            expected,
            "image replacement/deletion",
        );
    }
    drop(cx);
    drop(client);
}

struct ImagePreview {
    frame: Arc<GridFrame>,
    preview: phux_desktop::terminal::Preview,
}

impl gpui::Render for ImagePreview {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        self.preview
            .element(Arc::clone(&self.frame), settings(false))
    }
}

fn preview_pixels(payload: &[u8], extent: f32) -> RgbaImage {
    let (client, view) = publish_at(payload, 4, 2);
    let frame = frame_of(&client, view);
    let mut cx = gpu();
    let window = cx
        .open_window(size(px(extent), px(extent)), move |_, cx| {
            cx.new(|_| ImagePreview {
                frame,
                preview: Default::default(),
            })
        })
        .expect("preview");
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
        .expect("preview draw");
    let image = cx
        .capture_screenshot(window.into())
        .expect("preview pixels");
    drop(cx);
    drop(client);
    image
}

pub(super) fn kitty_placeholder_sampling_matches_classic_in_fractional_previews() {
    const CLASSIC: &[u8] =
        b"\x1b_Ga=T,f=32,s=2,v=2,c=4,r=2,i=42,C=1;/wAAgAAA/4D/AACAAAD/gA==\x1b\\";
    const VIRTUAL: &[u8] = concat!(
        "\x1b_Ga=T,f=32,s=2,v=2,c=4,r=2,i=42,U=1,C=1;/wAAgAAA/4D/AACAAAD/gA==\x1b\\",
        "\x1b[38;5;42m\u{10eeee}\u{305}\u{305}\u{10eeee}\u{10eeee}\u{10eeee}",
        "\x1b[2;1H\u{10eeee}\u{30d}\u{305}\u{10eeee}\u{10eeee}\u{10eeee}\x1b[m",
    )
    .as_bytes();
    for extent in [64.0, 3.6] {
        let classic = preview_pixels(CLASSIC, extent);
        let virtual_image = preview_pixels(VIRTUAL, extent);
        assert_eq!(classic.dimensions(), virtual_image.dimensions());
        assert!(
            classic.pixels().any(|p| p[0] > 100 && p[2] < 80),
            "red gradient endpoint"
        );
        assert!(
            classic.pixels().any(|p| p[2] > 100 && p[0] < 80),
            "blue gradient endpoint"
        );
        for (index, (a, b)) in classic.pixels().zip(virtual_image.pixels()).enumerate() {
            assert!(
                a.0.iter().zip(b.0).all(|(a, b)| a.abs_diff(b) <= 2),
                "{extent}px preview at pixel {index}: classic {a:?}, placeholder {b:?}"
            );
        }
    }
}
