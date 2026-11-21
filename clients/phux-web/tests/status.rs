//! Agent badges speak the shared status vocabulary: the
//! ladder from `phux_design::tone_of`, a distinct mark per shape class, the
//! vocabulary's words, and announcements only for needs-you and failed.

use phux_design::State;
use phux_web::PaneExit;
use phux_web::client::{announcement, ladder, mark};
use wasm_bindgen_test::wasm_bindgen_test;

const fn exit(code: Option<i32>, signal: Option<i32>) -> Option<PaneExit> {
    Some(PaneExit { code, signal })
}

#[wasm_bindgen_test]
fn kernel_words_map_onto_the_ladder() {
    assert_eq!(ladder("working", None, false, false), State::Working);
    assert_eq!(ladder("blocked", None, false, false), State::NeedsYou);
    assert_eq!(ladder("idle", None, false, false), State::Idle);
    assert_eq!(ladder("done", None, false, false), State::Done);
    assert_eq!(ladder("done", None, false, true), State::DoneSeen);
    assert_eq!(ladder("unknown", None, false, false), State::Unknown);
    assert_eq!(ladder("", None, false, false), State::Unknown);
    assert_eq!(ladder("ended", None, false, false), State::Ended);
}

#[wasm_bindgen_test]
fn a_pane_exit_and_the_transport_decide_failed_and_lost() {
    assert_eq!(
        ladder("working", exit(Some(1), None), false, false),
        State::Failed
    );
    assert_eq!(
        ladder("working", exit(None, Some(9)), false, false),
        State::Failed
    );
    assert_eq!(
        ladder("working", exit(Some(0), None), false, false),
        State::Done
    );
    assert_eq!(
        ladder("unknown", exit(Some(0), None), false, false),
        State::Ended
    );
    assert_eq!(ladder("blocked", None, true, false), State::Lost);
    assert_eq!(State::Lost.word(), Some("lost"));
}

#[wasm_bindgen_test]
fn the_quiet_class_shares_a_mark_but_never_a_name() {
    assert_eq!(mark(State::DoneSeen), mark(State::Idle));
    assert_ne!(State::DoneSeen.word(), State::Idle.word());
    let loud = [
        State::NeedsYou,
        State::Failed,
        State::Done,
        State::Working,
        State::Idle,
        State::Unknown,
        State::Ended,
    ];
    for (i, a) in loud.iter().enumerate() {
        for b in &loud[i + 1..] {
            assert_ne!(mark(*a), mark(*b), "{a:?} and {b:?} differ by shape");
        }
    }
}

#[wasm_bindgen_test]
fn only_needs_you_and_failed_are_announced() {
    assert_eq!(
        announcement("claude", State::NeedsYou).as_deref(),
        Some("claude needs you")
    );
    assert_eq!(
        announcement("codex", State::Failed).as_deref(),
        Some("codex failed")
    );
    assert_eq!(announcement("pi", State::Lost).as_deref(), Some("pi lost"));
    for quiet in [State::Working, State::Done, State::Idle, State::Unknown] {
        assert_eq!(announcement("claude", quiet), None);
    }
}
