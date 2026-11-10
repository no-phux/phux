use super::*;
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::wire::frame::FrameKind;

/// The server's fresh cut after applying the client's proposed PTY size.
/// Put a prompt on its last row, where overlay controls previously hid it.
fn resized_prompt(client: &phux_client_runtime::Client, cols: u16, rows: u16) {
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(2).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: id(1),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols,
            rows,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: id(1),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: format!("\x1b[2J\x1b[{rows};1HPROMPT_READY> ")
                .into_bytes()
                .into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: id(1),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        client.feed(frame).unwrap();
    }
}

fn assert_viewport(rt: &rune::Runtime<Desk>, rows: u16) {
    let viewport = geometry(rt, "phone-spatial-stage").rect;
    let toolbar = geometry(rt, "phone-navigation-toolbar").rect;
    let map = geometry(rt, "phone-minimap").rect;
    assert_eq!(toolbar[3], 44.0, "reserved touch toolbar");
    assert!(
        viewport[1] + viewport[3] <= toolbar[1] + 0.5,
        "toolbar must not cover the canvas"
    );
    assert!(
        map[1] + map[3] <= viewport[1],
        "map belongs in the header, above all terminal text"
    );
    let area = rt.state.pane_area(&id(1)).unwrap();
    assert!((area[1] - (viewport[3] - 2.0 * crate::ui::PAD)).abs() < 0.5);
    assert_eq!(
        rt.state.phone.as_ref().unwrap().stage,
        [viewport[2], viewport[3]]
    );
    let grid_top = rt.state.origins[&id(1)][1];
    let last_row_bottom = grid_top + f32::from(rows) * rt.state.metrics().cell_h;
    assert!(
        last_row_bottom <= toolbar[1] - crate::ui::PAD + 0.5,
        "every proposed row fits above the toolbar"
    );
    for key in ["phone-navigation", "phone-pane-picker"] {
        let action = geometry(rt, key).rect;
        assert_eq!(action[3], 44.0);
        assert!(action[1] >= toolbar[1] && action[1] + action[3] <= toolbar[1] + toolbar[3] + 0.5);
    }
    assert!(
        rt.find("phone-more").is_none(),
        "multi-pane map replaces redundant More"
    );
    assert!(
        rt.find("phone-command").is_some(),
        "accessory still provides Commands"
    );
}

#[test]
fn phone_sizes_reserve_toolbar_and_header_space_so_the_last_prompt_stays_visible() {
    for size in [(393.0, 852.0), (320.0, 568.0)] {
        for keyboard in [true, false] {
            let (client, mut rt) = fixture(true);
            rt.set_viewport(size, 2.0);
            rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = keyboard);
            settle(&mut rt);
            let ((cols, rows), _) = rt.state.fitted[&id(1)];
            assert_viewport(&rt, rows);
            resized_prompt(&client, cols, rows);
            poll(&mut rt);
            settle(&mut rt);
            let prompt = rt
                .elements()
                .into_iter()
                .find(|e| {
                    e.text
                        .as_deref()
                        .is_some_and(|s| s.contains("PROMPT_READY>"))
                })
                .expect("actual last-row prompt rendered");
            let viewport = geometry(&rt, "phone-spatial-stage").rect;
            let toolbar = geometry(&rt, "phone-navigation-toolbar").rect;
            assert!(
                prompt.visible[2] > 0.0 && prompt.visible[3] > 10.0,
                "prompt must not be clipped: {prompt:?}"
            );
            assert!(prompt.rect[1] >= viewport[1]);
            assert!(
                prompt.rect[1] + prompt.rect[3] <= toolbar[1],
                "prompt must not be beneath any action"
            );
            no_input(&client);
        }
    }
}

#[test]
fn a_single_terminal_keeps_more_and_gives_its_whole_stage_to_the_terminal() {
    let (client, mut rt) = fixture(true);
    tap(&mut rt, "phone-tab-1");
    poll(&mut rt);
    settle(&mut rt);
    assert!(rt.find("phone-minimap").is_none());
    assert!(rt.find("phone-more").is_some());
    assert!(rt.find("phone-navigation").is_none());
    assert!(rt.find("phone-pane-picker").is_none());
    let toolbar = geometry(&rt, "phone-navigation-toolbar").rect;
    let viewport = geometry(&rt, "phone-spatial-stage").rect;
    assert_eq!(toolbar[3], 0.0, "nothing to move between: no toolbar");
    assert!(viewport[1] + viewport[3] <= toolbar[1] + 0.5);
    no_input(&client);
}
