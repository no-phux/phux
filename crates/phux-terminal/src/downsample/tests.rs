use super::*;
use phux_protocol::caps::{ImageProtocolSet, KeyboardProtocolSet};

#[test]
fn color_tier_rewrites() {
    use ColorSupport::{Indexed16 as I16, Indexed256 as I256, TrueColor as TC};
    let cases: &[(&[u8], ColorSupport, &[u8])] = &[
        (
            b"\x1b[38;2;255;0;0mhello\x1b[0m world",
            TC,
            b"\x1b[38;2;255;0;0mhello\x1b[0m world",
        ),
        (
            b"hello world\nplain ASCII\r\n",
            I16,
            b"hello world\nplain ASCII\r\n",
        ),
        (
            b"hello world\nplain ASCII\r\n",
            I256,
            b"hello world\nplain ASCII\r\n",
        ),
        (b"\x1b[38;2;255;0;0mX", I256, b"\x1b[38;5;196mX"),
        (b"\x1b[48;2;0;0;255mZ", I256, b"\x1b[48;5;21mZ"),
        (b"\x1b[38;2;255;0;0mX", I16, b"\x1b[91mX"),
        (b"\x1b[48;2;255;0;0mY", I16, b"\x1b[101mY"),
        (b"\x1b[38;2;128;0;0mX", I16, b"\x1b[31mX"),
        // Lab, not RGB distance: dark cyan is not black, dim blue is dim.
        (b"\x1b[38;2;0;64;64mX", I16, b"\x1b[36mX"),
        (b"\x1b[48;2;0;40;192mX", I16, b"\x1b[44mX"),
        (b"\x1b[1;38;2;255;0;0;4mX", I256, b"\x1b[1;38;5;196;4mX"),
        (
            b"\x1b[2J\x1b[Hhello\x1b[31m!",
            I16,
            b"\x1b[2J\x1b[Hhello\x1b[31m!",
        ),
        (
            b"\x1b]0;hello world\x1b\\rest",
            I16,
            b"\x1b]0;hello world\x1b\\rest",
        ),
        (b"\x1b]2;title\x07rest", I256, b"\x1b]2;title\x07rest"),
        (b"\x1b[mX", I256, b"\x1b[mX"),
        (b"abc\x1b", I256, b"abc\x1b"),
        (b"abc\x1b[38;2;1", I256, b"abc\x1b[38;2;1"),
        (b"\x1b=foo", I256, b"\x1b=foo"),
        // ITU colon forms: empty, explicit, and absent colour-space slot.
        (b"\x1b[38:2::255:0:0mX", I256, b"\x1b[38;5;196mX"),
        (b"\x1b[48:2::255:0:0mY", I16, b"\x1b[101mY"),
        (b"\x1b[38:2:0:255:0:0mX", I256, b"\x1b[38;5;196mX"),
        (b"\x1b[38:2:255:0:0mX", I256, b"\x1b[38;5;196mX"),
        (b"\x1b[38:2::255:0:0mX", TC, b"\x1b[38:2::255:0:0mX"),
        // Curly underline is not a color.
        (b"\x1b[4:3mX", I256, b"\x1b[4:3mX"),
    ];
    for (input, tier, want) in cases {
        let caps = ClientCapabilities::new().with_color_support(*tier);
        assert_eq!(
            rewrite_bytes_with_caps(input, caps),
            *want,
            "{input:?} at {tier:?}"
        );
    }
}

#[test]
fn capability_gated_escapes() {
    let all = ClientCapabilities::new();
    let no_links = ClientCapabilities::new().with_hyperlinks(false);
    let no_images = ClientCapabilities::new().with_image_protocols(ImageProtocolSet::new());
    let no_kbd = ClientCapabilities::new().with_kbd_protocols(KeyboardProtocolSet::new());
    let no_kitty_graphics =
        ClientCapabilities::new().with_image_protocols(ImageProtocolSet::with(&[
            ImageProtocol::Sixel,
            ImageProtocol::Iterm2,
        ]));
    let osc8: &[u8] = b"\x1b]8;;https://example.com\x1b\\hello\x1b]8;;\x1b\\";
    let sixel: &[u8] = b"prefix\x1bP0;0;0q#0;2;100;0;0~~~\x1b\\suffix";
    let kitty_gfx: &[u8] = b"start\x1b_Ga=T,f=24;payload\x1b\\end";
    let iterm2: &[u8] = b"a\x1b]1337;File=name=test:AAAA\x1b\\b";
    let kbd: &[u8] = b"head\x1b_13;2u\x1b\\tail";
    let cases: &[(&[u8], ClientCapabilities, &[u8])] = &[
        (osc8, all, osc8),
        (osc8, no_links, b"hello"),
        (
            b"\x1b]8;;https://x.example\x07link text\x1b]8;;\x07trailing",
            no_links,
            b"link texttrailing",
        ),
        (
            b"\x1b]0;window title\x1b\\rest",
            no_links,
            b"\x1b]0;window title\x1b\\rest",
        ),
        (sixel, all, sixel),
        (sixel, no_images, b"prefixsuffix"),
        // DECRQSS (`$ q`) is not sixel.
        (b"\x1bP$q\"p\x1b\\", no_images, b"\x1bP$q\"p\x1b\\"),
        (kitty_gfx, all, kitty_gfx),
        (kitty_gfx, no_images, b"startend"),
        (iterm2, all, iterm2),
        (iterm2, no_images, b"ab"),
        (kbd, all, kbd),
        (kbd, no_kbd, b"headtail"),
        (b"x\x1b_Ga=T;abc\x1b\\y", no_kbd, b"x\x1b_Ga=T;abc\x1b\\y"),
        (
            b"x\x1b_13;2u\x1b\\y",
            no_kitty_graphics,
            b"x\x1b_13;2u\x1b\\y",
        ),
    ];
    for (i, (input, caps, want)) in cases.iter().enumerate() {
        assert_eq!(rewrite_bytes_with_caps(input, *caps), *want, "case {i}");
    }
}

#[test]
fn plain_runs_preserve_binary_bytes_and_escape_boundaries() {
    let plain = b"text \xe6\x9d\xb1\xe4\xba\xac\x00\xff\x9b\r\n".repeat(80);
    let caps = ClientCapabilities::new()
        .with_color_support(ColorSupport::Indexed256)
        .with_image_protocols(ImageProtocolSet::new());
    for tail in [b"".as_slice(), b"\x1b", b"\x1b[38;2;255"] {
        let input = [
            plain.as_slice(),
            b"\x1b[38;2;255;0;0m",
            plain.as_slice(),
            b"\x1b_Ga=T;image\x1b\\",
            plain.as_slice(),
            tail,
        ]
        .concat();
        let expected = [
            plain.as_slice(),
            b"\x1b[38;5;196m",
            plain.as_slice(),
            plain.as_slice(),
            tail,
        ]
        .concat();
        assert_eq!(rewrite_bytes_with_caps(&input, caps), expected);
    }
}

#[test]
fn color_downgrade_and_image_strip_compose() {
    let input = b"\x1b[38;2;255;0;0mhi\x1bP0;0;0qsixel\x1b\\done";
    let caps = ClientCapabilities::new()
        .with_color_support(ColorSupport::Indexed256)
        .with_image_protocols(ImageProtocolSet::with(&[
            ImageProtocol::KittyGraphics,
            ImageProtocol::Iterm2,
        ]));
    assert_eq!(
        rewrite_bytes_with_caps(input, caps),
        b"\x1b[38;5;196mhidone"
    );
}
