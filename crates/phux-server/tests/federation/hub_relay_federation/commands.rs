//! Commands the hub tests address to satellite resources.

use super::*;

pub(super) fn on_satellite(host: &str, spawn: Spawn) -> Spawn {
    Spawn {
        satellite: Some(SatelliteHost::new(host)),
        ..spawn
    }
}

/// `REPORT_ASKED` with no suggestions or elapsed time.
pub(super) fn report_asked(terminal: &ResourceId, id: &str, question: &str) -> Command {
    Command::ReportAsked {
        terminal_id: terminal.clone(),
        id: id.to_owned(),
        question: question.to_owned(),
        suggestions: Vec::new(),
        elapsed_seconds: None,
    }
}

pub(super) const fn focus_gained(terminal_id: ResourceId) -> Command {
    phux_server_testkit::route_input(terminal_id, InputEvent::Focus(FocusEvent::Gained))
}
