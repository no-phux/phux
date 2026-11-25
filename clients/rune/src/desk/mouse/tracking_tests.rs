//! Real embedded replicas and Rune transactions, never an installed server.
use std::sync::Arc;

use rune::{Runtime, WheelEvent};

use super::*;
use phux_protocol::wire::frame::FrameKind;

fn fixture(pixels: bool) -> (Arc<Client>, Runtime<Desk>, ResourceId) {
    let extra = if pixels { "\x1b[?1016h" } else { "" };
    let (client, mut rt) = crate::test_support::input_fixture_showing(&format!(
        "\x1b[?1049h\x1b[H\x1b[?1003h\x1b[?1006h{extra}words to select"
    ));
    rt.update(|_, tx| tx.instant());
    for _ in 0..120 {
        rt.frame(1.0 / 60.0);
    }
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    assert_eq!(
        client.engine().unwrap().pixel_mouse(&terminal).unwrap(),
        pixels
    );
    let _ = client.take_outbound();
    (client, rt, terminal)
}

fn mouse(client: &Client) -> Vec<MouseEvent> {
    client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match phux_wire_fixtures::decode_one(bytes) {
            FrameKind::InputMouse { event, .. } => Some(event),
            _ => None,
        })
        .collect()
}

fn press(phase: Phase, x: f32, y: f32) -> Press {
    Press {
        phase,
        x,
        y,
        clicks: 1,
        mods: Mods::default(),
        button: PointerButton::Primary,
    }
}

#[test]
fn clicks_drags_and_releases_use_the_requested_precision() {
    for pixels in [false, true] {
        let (client, mut rt, terminal) = fixture(pixels);
        let m = rt.state.metrics();
        let (cw, ch) = m.cell_px();
        let (x, y) = (m.cell_w + 2.25 / m.scale, m.cell_h + 3.25 / m.scale);
        rt.update(|d, tx| {
            d.pointer(&terminal, &press(Phase::Down, x, y), tx);
            d.pointer(&terminal, &press(Phase::Move, x + 1.0 / m.scale, y), tx);
            d.pointer(&terminal, &press(Phase::Move, x + 1.0 / m.scale, y), tx);
            d.pointer(&terminal, &press(Phase::Up, x + 1.0 / m.scale, y), tx);
        });
        let events = mouse(&client);
        let actions: Vec<_> = events.iter().map(|event| event.action).collect();
        if pixels {
            assert_eq!(
                actions,
                [
                    MouseAction::Press,
                    MouseAction::Motion,
                    MouseAction::Release
                ]
            );
            assert_eq!(
                (events[0].x, events[0].y),
                (f64::from(cw + 2), f64::from(ch + 3))
            );
            assert_eq!(events[1].x, f64::from(cw + 3));
        } else {
            assert_eq!(actions, [MouseAction::Press, MouseAction::Release]);
            assert_eq!((events[0].x, events[0].y), cell_center(&m, 1, 1));
        }
        assert!(rt.state.capture_terminal().is_none());
    }
}

#[test]
fn hover_deduplicates_at_app_precision_but_keeps_modifier_changes() {
    for pixels in [false, true] {
        let (client, mut rt, terminal) = fixture(pixels);
        let m = rt.state.metrics();
        let (x, y) = (m.cell_w + 2.25 / m.scale, m.cell_h + 3.25 / m.scale);
        let moved = x + 1.0 / m.scale;
        rt.update(|d, _| {
            d.report_motion(&terminal, x, y, Mods::default());
            d.report_motion(&terminal, x + 0.1 / m.scale, y, Mods::default());
            d.report_motion(&terminal, moved, y, Mods::default());
            d.report_motion(&terminal, moved, y, Mods::default());
            d.report_motion(
                &terminal,
                moved,
                y,
                Mods {
                    ctrl: true,
                    ..Mods::default()
                },
            );
            d.report_motion(&terminal, -1.0, y, Mods::default());
            d.report_motion(
                &terminal,
                moved,
                y,
                Mods {
                    shift: true,
                    ..Mods::default()
                },
            );
        });
        let events = mouse(&client);
        assert_eq!(events.len(), if pixels { 3 } else { 2 });
        assert!(
            events
                .iter()
                .all(|event| event.action == MouseAction::Motion)
        );
        assert_eq!(events.last().unwrap().mods, ModSet::CTRL);
    }
}

#[test]
fn wheel_uses_exact_pixels_only_when_requested() {
    for pixels in [false, true] {
        let (client, mut rt, terminal) = fixture(pixels);
        let m = rt.state.metrics();
        let (cw, ch) = m.cell_px();
        let (x, y) = (m.cell_w + 2.25 / m.scale, m.cell_h + 3.25 / m.scale);
        rt.update(|d, _| d.wheel(&terminal, x, y, m.cell_h, Mods::default()));
        let events = mouse(&client);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].button, MouseButton::Four);
        let expected = if pixels {
            (f64::from(cw + 2), f64::from(ch + 3))
        } else {
            cell_center(&m, 1, 1)
        };
        assert_eq!((events[0].x, events[0].y), expected);
    }
}

#[test]
fn native_pointer_and_wheel_routing_retain_subcell_positions() {
    let (client, mut rt, terminal) = fixture(true);
    let m = rt.state.metrics();
    let [left, top] = rt.state.connection.origins[&terminal];
    let (x, y) = (
        left + m.cell_w * 10.0 + 2.25 / m.scale,
        top + m.cell_h * 5.0 + 3.25 / m.scale,
    );
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_move(x + 1.0 / m.scale, y);
    rt.pointer_up(x + 1.0 / m.scale, y);
    rt.wheel_event(WheelEvent::new(0.0, m.cell_h).at(x, y));
    rt.frame(0.0);
    let events = mouse(&client);
    assert!(
        events
            .iter()
            .any(|event| event.action == MouseAction::Motion),
        "{events:?}, origin={:?}, pane={:?}, modal={:?}",
        [left, top],
        crate::ui::element(&rt, "pane-@1").rect,
        rt.state.modal(),
    );
    assert!(
        events
            .iter()
            .any(|event| event.action == MouseAction::Release)
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.button, MouseButton::Four | MouseButton::Five))
    );
    let (cw, ch) = m.cell_px();
    for event in events {
        assert!(
            [f64::from(cw * 10 + 2), f64::from(cw * 10 + 3)].contains(&event.x),
            "{event:?}"
        );
        assert_eq!(event.y, f64::from(ch * 5 + 3));
    }
}

#[test]
fn shift_keeps_local_selection_even_with_pixel_tracking() {
    let (client, mut rt, terminal) = fixture(true);
    let m = rt.state.metrics();
    rt.update(|d, tx| {
        for (phase, x) in [
            (Phase::Down, m.cell_w),
            (Phase::Move, m.cell_w * 6.0),
            (Phase::Up, m.cell_w * 6.0),
        ] {
            let mut event = press(phase, x, m.cell_h / 2.0);
            event.mods.shift = true;
            d.pointer(&terminal, &event, tx);
        }
    });
    assert!(mouse(&client).is_empty());
    assert!(
        !client
            .engine()
            .unwrap()
            .view_selection_text(rt.state.view_of(&terminal).unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn precision_changes_and_panes_do_not_share_a_motion_dedup_key() {
    let (_client, mut rt, terminal) = fixture(true);
    rt.update(|d, _| {
        let remember = |d: &mut Desk, id: &ResourceId, position| {
            d.remember_position(id, position, ModSet::empty(), MouseButton::Unknown)
        };
        assert!(remember(d, &terminal, Position::Pixel(1, 1)));
        assert!(!remember(d, &terminal, Position::Pixel(1, 1)));
        assert!(remember(d, &ResourceId::local(2), Position::Pixel(1, 1)));
        assert!(remember(d, &terminal, Position::Cell(1, 1)));
    });
}

#[test]
fn captured_release_clamps_to_the_grid_and_mode_reset_restores_cells() {
    let (client, mut rt, terminal) = fixture(true);
    rt.update(|d, tx| {
        d.pointer(&terminal, &press(Phase::Down, 3.0, 4.0), tx);
        d.pointer(&terminal, &press(Phase::Up, -20.0, 1e6), tx);
    });
    let events = mouse(&client);
    let m = rt.state.metrics();
    let frame = client
        .acquire_view(rt.state.view_of(&terminal).unwrap())
        .unwrap();
    assert_eq!(events[1].x, 0.0);
    assert_eq!(
        events[1].y,
        f64::from(u32::from(frame.rows) * u32::from(m.cell_px().1) - 1)
    );
    client
        .feed(phux_wire_fixtures::output(&terminal, 2, b"\x1b[?1016l"))
        .unwrap();
    rt.update(|d, _| d.report_motion(&terminal, m.cell_w * 1.25, m.cell_h * 1.25, Mods::default()));
    let events = mouse(&client);
    assert_eq!((events[0].x, events[0].y), cell_center(&m, 1, 1));
}
