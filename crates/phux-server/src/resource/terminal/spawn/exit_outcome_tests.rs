use std::os::unix::process::ExitStatusExt;

use super::{ExitOutcome, exit_outcome};

fn std_death_by(signal: i32) -> portable_pty::ExitStatus {
    portable_pty::ExitStatus::from(std::process::ExitStatus::from_raw(signal))
}

/// A signal death reports the signal, not "no status".
#[test]
fn signal_death_is_reported_as_signal_not_none() {
    assert_eq!(exit_outcome(&std_death_by(9)), ExitOutcome::signaled(9));
    assert_eq!(exit_outcome(&std_death_by(15)), ExitOutcome::signaled(15));
    // The adopted-child rendering maps back too.
    assert_eq!(
        exit_outcome(&portable_pty::ExitStatus::with_signal("signal 2")),
        ExitOutcome::signaled(2)
    );
}

#[test]
fn exit_codes_stay_codes() {
    assert_eq!(
        exit_outcome(&portable_pty::ExitStatus::with_exit_code(0)),
        ExitOutcome::exited(0)
    );
    assert_eq!(
        exit_outcome(&portable_pty::ExitStatus::with_exit_code(42)),
        ExitOutcome::exited(42)
    );
}

/// A name that is not a signal (the adopted child's `ECHILD` sentinel)
/// is unknown, never a fabricated code or signal.
#[test]
fn a_non_signal_name_is_unknown() {
    assert_eq!(
        exit_outcome(&portable_pty::ExitStatus::with_signal(
            portable_pty_adopt::ECHILD_EXIT_SIGNAL_NAME
        )),
        ExitOutcome::UNKNOWN
    );
}
