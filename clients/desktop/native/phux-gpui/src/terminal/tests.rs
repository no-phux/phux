use super::{FocusStep, focus_step};

#[test]
fn focus_follows_the_shell_flag_edges() {
    assert_eq!(focus_step(true, false, false, false), FocusStep::Focus);
    assert_eq!(focus_step(true, true, false, false), FocusStep::Keep);
    assert_eq!(focus_step(false, true, false, true), FocusStep::Blur);
    assert_eq!(focus_step(false, false, false, true), FocusStep::Keep);
}

#[test]
fn a_rebound_input_takes_focus_the_shell_still_wants() {
    assert_eq!(focus_step(true, true, true, false), FocusStep::Focus);
    assert_eq!(focus_step(true, true, true, true), FocusStep::Keep);
    assert_eq!(focus_step(false, false, true, false), FocusStep::Keep);
}
