//! Output pacing: what settles the withheld debt and arms the reply grace.

/// An admitted burst settles the pacer's debt itself: its deadline was just
/// pushed out and a saturating socket starves the timer arm.
#[test]
fn an_admitted_burst_settles_the_withheld_debt() {
    use super::super::loop_state::burst_settles_debt;
    assert!(burst_settles_debt(true, false));
    assert!(burst_settles_debt(false, true));
    assert!(!burst_settles_debt(false, false));
}

/// Pointer motion must not arm the reply grace (a drag would keep pacing off
/// for the whole client); press and release do.
#[test]
fn pointer_motion_does_not_arm_the_reply_grace() {
    use phux_protocol::input::InputEvent;
    use phux_protocol::input::key::ModSet;
    use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
    let expects = |action| {
        super::super::loop_state::input_expects_a_reply(&InputEvent::Mouse(MouseEvent {
            action,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: 4.0,
            y: 2.0,
        }))
    };
    assert!(!expects(MouseAction::Motion));
    assert!(expects(MouseAction::Press) && expects(MouseAction::Release));
}
