//! Bell, wide-character copy and focus reporting, through the live client.

use super::*;

#[wasm_bindgen_test]
async fn the_bell_announces_itself_and_flashes_the_canvas() {
    let canvas = mounted_canvas("bell-canvas");
    let rung = Rc::new(Cell::new(0));
    let count = Rc::clone(&rung);
    let listener = Closure::<dyn FnMut()>::new(move || count.set(count.get() + 1));
    canvas
        .add_event_listener_with_callback(
            phux_web::client::BELL_EVENT,
            listener.as_ref().unchecked_ref(),
        )
        .unwrap();
    let client = connect(&canvas).await;
    canvas.focus().unwrap();
    let surface = input_surface(&canvas);
    sleep(Duration::from_millis(300)).await;
    let background = cell_pixel(&canvas, 79, 23);
    assert_eq!(rung.get(), 0, "attaching does not ring");

    // Ctrl+G is BEL: the TTY echoes `^G`, then cat rings it.
    fresh_line(&surface);
    ctrl_key(&surface, "g", "KeyG");
    enter(&surface);
    let mut flashed = false;
    for _ in 0..POLLS * 4 {
        flashed |= rung.get() > 0 && cell_pixel(&canvas, 79, 23) != background;
        if flashed {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(rung.get(), 1, "one phux-bell event");
    assert!(flashed, "the canvas flashed");
    sleep(Duration::from_millis(400)).await;
    assert_eq!(cell_pixel(&canvas, 79, 23), background, "the flash ends");
    client.close();
}

#[wasm_bindgen_test]
async fn a_double_width_character_copies_without_a_trailing_space() {
    let (canvas, client, surface) = connected("wide-canvas").await;
    canvas.focus().unwrap();
    fresh_line(&surface);
    type_text(&surface, "\u{4e2d}\u{6587}Z");
    assert!(wait_for(&client, "\u{4e2d}\u{6587}Z").await);
    let rows = client.rows_text();
    let (row, first) = rows
        .iter()
        .enumerate()
        .find_map(|(row, text)| {
            text.chars()
                .position(|ch| ch == '\u{4e2d}')
                .map(|col| (row as u16, col as u16))
        })
        .unwrap();
    // Cells: the two wide characters and their spacers, then "Z".
    let select = |last: u16| {
        pointer(&canvas, "pointerdown", first, row);
        pointer(&canvas, "pointermove", last, row);
        pointer(&canvas, "pointerup", last, row);
        copy_event(&surface)
    };
    assert_eq!(select(first + 4), copied("\u{4e2d}\u{6587}Z"));
    assert_eq!(
        select(first + 1),
        copied("\u{4e2d}"),
        "a wide character alone copies without its spacer"
    );
    enter(&surface);
    client.close();
}

#[wasm_bindgen_test]
async fn focus_changes_reach_a_program_that_asks_for_them() {
    let (canvas, client, surface) = connected("focus-canvas").await;
    canvas.focus().unwrap();
    let surface: web_sys::HtmlElement = surface.dyn_into().unwrap();
    program_prints(&client, &surface, &["[?1004h"], "FOCUS_ON", &[]).await;

    // The server writes CSI O and CSI I to the pane; the TTY echoes them.
    let reports = || (occurrences(&client, "^[[O"), occurrences(&client, "^[[I"));
    let (lost, gained) = reports();
    surface.blur().unwrap();
    assert!(
        wait_more(&client, "^[[O", lost).await,
        "focus out: {}",
        screen(&client)
    );
    surface.focus().unwrap();
    assert!(
        wait_more(&client, "^[[I", gained).await,
        "focus in: {}",
        screen(&client)
    );

    program_prints(&client, &surface, &["[?1004l"], "FOCUS_OFF", &[]).await;
    let before = reports();
    surface.blur().unwrap();
    surface.focus().unwrap();
    sleep(Duration::from_millis(300)).await;
    assert_eq!(
        reports(),
        before,
        "no reports once the program stops asking: {}",
        screen(&client)
    );
    client.close();
}
