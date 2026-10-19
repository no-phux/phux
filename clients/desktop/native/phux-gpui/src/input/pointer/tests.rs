use super::release::{ReleaseFacts, ReleaseFrame, ReleaseValidation, refuse_captured_release};
use super::selection::autoscroll_rows;
use super::wheel::smooth_target;
use super::{InputError, repeats_cell};
use gpui::{point, px, size};

/// Inside the grid nothing scrolls; past an edge it scrolls toward that
/// edge, faster the farther the pointer is.
#[test]
fn autoscroll_speed_follows_the_distance_past_the_edge() {
    let bounds = gpui::Bounds::new(point(px(0.), px(100.)), size(px(80.), px(68.)));
    let rows = |y: f32| autoscroll_rows(bounds, px(17.), px(y));
    assert_eq!(rows(120.), 0);
    assert_eq!(rows(99.), -1);
    assert_eq!(rows(70.), -2);
    assert_eq!(rows(169.), 1);
    assert_eq!(rows(168. + 17. * 40.), 8, "capped");
}

#[test]
fn smooth_target_keeps_a_continuous_position_inside_the_scrollback() {
    let near = |(row, fraction): (u64, f32), expected: (u64, f32)| {
        row == expected.0 && (fraction - expected.1).abs() < 1e-4
    };
    // Up from the tail by a quarter row shows the row above, mostly scrolled past.
    assert!(near(smooth_target(10, 0., 10, 0.25), (9, 0.75)));
    // Down within a row only grows the fraction.
    assert!(near(smooth_target(9, 0.25, 10, -0.5), (9, 0.75)));
    // Crossing rows carries the remainder.
    assert!(near(smooth_target(9, 0.5, 10, 2.75), (6, 0.75)));
    // The top and the live tail clamp, and the tail has no fraction.
    assert!(near(smooth_target(1, 0.5, 10, 5.), (0, 0.)));
    assert!(near(smooth_target(9, 0.5, 10, -3.), (10, 0.)));
    // No scrollback: nothing moves.
    assert!(near(smooth_target(0, 0., 0, 1.), (0, 0.)));
}

#[test]
fn motion_in_the_same_cell_is_one_event() {
    assert!(!repeats_cell(None, Some((0, 0))));
    assert!(repeats_cell(Some((0, 0)), Some((0, 0))));
    assert!(!repeats_cell(Some((0, 0)), Some((0, 1))));
    assert!(!repeats_cell(Some((1, 0)), Some((0, 0))));
    assert!(!repeats_cell(Some((0, 0)), None));
}

fn facts(
    validation: ReleaseValidation,
    epoch_matches: bool,
    frame: Option<ReleaseFrame>,
    input_ready: bool,
) -> ReleaseFacts {
    ReleaseFacts {
        validation,
        epoch_matches,
        frame,
        input_ready,
    }
}

#[test]
fn a_captured_release_survives_a_geometry_or_frame_gap() {
    let same = ReleaseFrame {
        terminal_matches: true,
        replica_matches: true,
    };
    assert_eq!(
        refuse_captured_release(facts(ReleaseValidation::Stale, true, Some(same), true)),
        None,
        "painted size behind the live frame"
    );
    assert_eq!(
        refuse_captured_release(facts(ReleaseValidation::Stale, true, None, true)),
        None,
        "no published frame"
    );
    assert_eq!(
        refuse_captured_release(facts(ReleaseValidation::Ready, true, Some(same), true)),
        None
    );
}

#[test]
fn a_captured_release_stops_for_replacement_or_a_fence() {
    let same = ReleaseFrame {
        terminal_matches: true,
        replica_matches: true,
    };
    let other_replica = ReleaseFrame {
        terminal_matches: true,
        replica_matches: false,
    };
    let other_terminal = ReleaseFrame {
        terminal_matches: false,
        replica_matches: true,
    };
    assert_eq!(
        refuse_captured_release(facts(ReleaseValidation::Stale, false, Some(same), true)),
        Some(InputError::StalePresentation)
    );
    assert_eq!(
        refuse_captured_release(facts(
            ReleaseValidation::Stale,
            true,
            Some(other_replica),
            true
        )),
        Some(InputError::StalePresentation)
    );
    assert_eq!(
        refuse_captured_release(facts(
            ReleaseValidation::Stale,
            true,
            Some(other_terminal),
            true
        )),
        Some(InputError::StalePresentation)
    );
    assert_eq!(
        refuse_captured_release(facts(ReleaseValidation::Stale, true, Some(same), false)),
        Some(InputError::NotReady)
    );
    assert_eq!(
        refuse_captured_release(facts(ReleaseValidation::Stale, true, None, false)),
        Some(InputError::NotReady)
    );
    assert_eq!(
        refuse_captured_release(facts(ReleaseValidation::NotReady, true, Some(same), false)),
        Some(InputError::NotReady)
    );
    assert_eq!(
        refuse_captured_release(facts(ReleaseValidation::Other, true, None, true)),
        Some(InputError::StalePresentation)
    );
}
