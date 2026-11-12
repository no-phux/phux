//! Sheet rules reserve exactly the device pixels their borders paint.

use super::*;
use rune::{Env, Runtime};

fn sheet(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let p = ink(cx.system_dark(), cx.accent(), desk.settings.theme);
    Sheet {
        key: "test-sheet",
        title: "Sheet",
        width: 420.0,
        top: 40.0,
        scrim: 0.2,
        close: |_| {},
    }
    .build(None, el().h(80.0), el().h(20.0), &p)
}

fn palette(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let p = ink(cx.system_dark(), cx.accent(), desk.settings.theme);
    super::palette::view(desk, &p, 40.0)
}

fn inbox(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let p = ink(cx.system_dark(), cx.accent(), desk.settings.theme);
    super::inbox::view(desk, &p)
}

#[test]
fn sheet_palette_and_inbox_rules_take_whole_device_pixels() {
    type Surface = fn(&Desk, &mut Cx<Desk>) -> El;
    let surfaces: [(Surface, &str); 3] = [(sheet, "sheet"), (palette, "palette"), (inbox, "inbox")];
    for scale in [1.0_f32, 1.25, 1.5, 2.0, 3.0] {
        for (phone, size) in [(false, (1100.0, 720.0)), (true, (393.0, 852.0))] {
            for (surface, name) in surfaces {
                let desk = Desk::settings_test_fixture().with_phone(phone);
                let mut rt = Runtime::new(desk, surface, size, Env::new(false, scale));
                rt.frame(0.0);
                for edge in ["header", "footer"] {
                    let key = format!("{name}-{edge}-rule");
                    if phone && name == "palette" && edge == "footer" {
                        assert!(rt.presented(&key).is_none(), "no phone chord footer");
                        continue;
                    }
                    let [_, _, w, h] = rt.presented(&key).expect("rule is laid out").0;
                    let expected = scale.floor().max(1.0) / scale;
                    assert!((h - expected).abs() < 0.01, "{key} at {scale}x: {h}");
                    assert!(w > 200.0, "{key} spans its panel: {w}");
                }
            }
        }
    }
}
