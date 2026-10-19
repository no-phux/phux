//! Releasing a mouse button a program captured, across a frame or geometry
//! gap.

use super::*;

impl TerminalInput {
    pub(super) fn send_captured_release(
        &self,
        button: WireButton,
        position: Point<Pixels>,
        modifiers: Modifiers,
    ) -> Result<(), InputError> {
        let surface = self.surface(position, true)?;
        let client = self.client()?;
        client.with_control(|control| {
            if let Some(error) = refuse_captured_release(self.release_facts(control)) {
                return Err(error);
            }
            self.send_mouse(control, MouseAction::Release, button, modifiers, surface)
        })
    }

    fn release_facts(&self, control: &ControlPlane) -> ReleaseFacts {
        let validation = match self.validate(control, true) {
            Ok(()) => ReleaseValidation::Ready,
            Err(InputError::StalePresentation) => ReleaseValidation::Stale,
            Err(InputError::NotReady) => ReleaseValidation::NotReady,
            Err(_) => ReleaseValidation::Other,
        };
        let epoch_matches = self
            .identity
            .is_some_and(|identity| control.connection_epoch() == identity.epoch);
        let frame = control
            .publication()
            .acquire_view(self.view)
            .map(|frame| ReleaseFrame {
                terminal_matches: frame.terminal_id == self.terminal,
                replica_matches: self.identity.is_some_and(|identity| {
                    (frame.stream_id, frame.bootstrap_id) == (identity.stream, identity.bootstrap)
                }) && control.engine().is_some_and(|engine| {
                    engine.view_replica_info(self.view).is_ok_and(|info| {
                        (info.stream_id, info.bootstrap_id) == (frame.stream_id, frame.bootstrap_id)
                    })
                }),
            });
        ReleaseFacts {
            validation,
            epoch_matches,
            frame,
            input_ready: input_ready(control, &self.terminal),
        }
    }
}

/// Why `validate` rejected or accepted a captured application-mouse release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReleaseValidation {
    Ready,
    Stale,
    NotReady,
    Other,
}

/// The live publication, when one exists, compared with the painted identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReleaseFrame {
    pub(super) terminal_matches: bool,
    pub(super) replica_matches: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReleaseFacts {
    pub(super) validation: ReleaseValidation,
    pub(super) epoch_matches: bool,
    /// `None` when the view has no published frame.
    pub(super) frame: Option<ReleaseFrame>,
    pub(super) input_ready: bool,
}

/// `None` means send the release.
///
/// A geometry gap (the live frame is this replica, but its size is not the
/// painted one yet) and an unpublished frame still belong to the captured
/// button. A replaced connection or an unknown-delivery fence does not.
pub(super) fn refuse_captured_release(facts: ReleaseFacts) -> Option<InputError> {
    match facts.validation {
        ReleaseValidation::Ready => None,
        ReleaseValidation::NotReady => Some(InputError::NotReady),
        ReleaseValidation::Other => Some(InputError::StalePresentation),
        ReleaseValidation::Stale => {
            let same_target = facts.epoch_matches
                && facts
                    .frame
                    .is_none_or(|frame| frame.terminal_matches && frame.replica_matches);
            if same_target && facts.input_ready {
                None
            } else if same_target {
                Some(InputError::NotReady)
            } else {
                Some(InputError::StalePresentation)
            }
        }
    }
}
