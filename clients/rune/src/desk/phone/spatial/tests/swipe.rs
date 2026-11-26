//! A sideways thumb pages between this session's tabs one to one, before
//! anything heavier steps over into another session.

use super::fixture::*;
use super::*;
use crate::desk::phone::swipe::Beside;
use crate::desk::phone::touch::Feedback;
use rune::Runtime;
use rune::TouchPhase::{Ended, Moved, Started};

fn at(rt: &mut Runtime<Desk>, phase: rune::TouchPhase, x: f32, y: f32, clock: f64) {
    rt.touch(9, phase, x, y, clock);
    rt.update(|d, tx| {
        d.poll(clock, tx);
    });
    rt.frame(1.0 / 60.0);
}

#[test]
fn a_sideways_thumb_pages_to_the_next_tab_one_to_one() {
    let (client, mut rt) = fixture(true);
    let session = rt.state.selected_session_name();
    assert_eq!(rt.state.page_target(-1.0), Some(Beside::Tab(1)));
    let before = rt.state.page_key();
    let stage = geometry(&rt, "phone-spatial-stage").rect;
    let (x, y) = (stage[0] + stage[2] * 0.6, stage[1] + stage[3] * 0.5);
    at(&mut rt, Started, x, y, 1.0);
    at(&mut rt, Moved, x - 60.0, y, 1.016);
    let swipe = &rt.state.phone.as_ref().unwrap().swipe;
    assert_eq!(swipe.page, -60.0, "a tab follows the finger exactly");
    assert!(rt.find("phone-page-peek").is_some(), "names the tab");
    at(&mut rt, Moved, x - 150.0, y, 1.032);
    assert_eq!(
        rt.state.phone.as_mut().unwrap().feedback.take(),
        Some(Feedback::Detent)
    );
    at(&mut rt, Ended, x - 150.0, y, 1.3);
    poll(&mut rt);
    settle(&mut rt);
    assert_eq!(focus(&rt), id(5), "on to the next tab");
    assert_eq!(rt.state.selected_session_name(), session, "same session");
    assert_ne!(rt.state.page_key(), before, "a page of its own");
    no_input(&client);
}
