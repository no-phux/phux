//! Coming back to a link that sat unused: the phone app returning to the
//! foreground, and typing into a connection that may have died unnoticed.

use std::time::{Duration, Instant};

use phux_client_runtime::Client;

use super::Desk;

/// The server reaps a connection this quiet (its QUIC idle timeout). Away
/// that long, the socket is gone whatever this side still believes.
const REAPED_AFTER: Duration = Duration::from_secs(30);

/// The least time between the probes typing sends, in seconds.
const NUDGE_EVERY: f64 = 1.0;

/// What coming back asks of a client that is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resume {
    /// The server has let go: dial again now, rather than probe a corpse.
    Redial,
    /// The socket may have survived on a new path: move to it and probe.
    Probe,
}

fn resume(away: Option<Duration>) -> Resume {
    match away {
        Some(away) if away >= REAPED_AFTER => Resume::Redial,
        _ => Resume::Probe,
    }
}

impl Desk {
    /// The app is in front again after `away`. Whatever the schedule had
    /// widened to while nobody was looking starts over, and the connection
    /// is tried now.
    pub(crate) fn resume_link(&mut self, away: Option<Duration>) {
        self.link.tries = 0;
        let alive = self
            .client
            .as_ref()
            .filter(|client| !client.status().is_terminal());
        let Some(client) = alive else {
            // An ended client is replaced on the next poll, unless only a
            // person can change the answer.
            if !self.stuck() {
                self.link.retry_at = Some(Instant::now());
            }
            return;
        };
        match resume(away) {
            Resume::Redial => client.resync(),
            Resume::Probe => client.network_path_changed(),
        }
    }

    /// A key went into the connection. One that died quietly still reads as
    /// attached, so probe it, at most once a second: the key's fate is then
    /// known within the probe's wait instead of the quiet clock's.
    pub(in crate::desk) fn nudge_typing(&mut self, client: &Client) {
        let since = self.link.nudged.map(|at| self.clock - at);
        if since.is_some_and(|since| (0.0..NUDGE_EVERY).contains(&since)) {
            return;
        }
        self.link.nudged = Some(self.clock);
        client.nudge();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use phux_client_runtime::Runtime;
    use phux_client_runtime::control::{ControlOptions, Status};

    use super::*;
    use crate::launch::{self, Options};

    fn desk() -> Desk {
        let options = Options {
            socket: Some(PathBuf::from("/nowhere/phux.sock")),
            ..Options::default()
        };
        Desk::open(&launch::resolve(&options), None)
    }

    #[test]
    fn a_long_absence_redials_and_a_short_one_probes() {
        assert_eq!(resume(None), Resume::Probe);
        assert_eq!(resume(Some(Duration::from_secs(29))), Resume::Probe);
        assert_eq!(resume(Some(REAPED_AFTER)), Resume::Redial);
        assert_eq!(resume(Some(Duration::from_secs(3600))), Resume::Redial);
    }

    #[test]
    fn coming_back_retries_an_ended_connection_now_from_the_first_rung() {
        let mut desk = desk();
        desk.status = Status::Failed;
        desk.link.tries = 6;
        desk.link.retry_at = Some(Instant::now() + Duration::from_secs(30));
        desk.resume_link(Some(Duration::from_secs(5)));
        assert_eq!(desk.link.tries, 0);
        assert!(desk.link.retry_at.is_some_and(|at| at <= Instant::now()));
        // Not scheduled yet is due all the same.
        desk.link.retry_at = None;
        desk.resume_link(None);
        assert!(desk.link.retry_at.is_some_and(|at| at <= Instant::now()));
    }

    #[test]
    fn coming_back_leaves_a_refusal_for_the_person() {
        let mut desk = desk();
        desk.status = Status::Failed;
        desk.error = Some("mini refused the pairing token (unauthorized)".to_owned());
        desk.link.tries = 3;
        desk.resume_link(Some(Duration::from_secs(120)));
        assert_eq!(desk.link.tries, 0);
        assert_eq!(desk.link.retry_at, None);
    }

    #[test]
    fn coming_back_to_a_running_client_keeps_it_and_its_schedule() {
        let mut desk = desk();
        let client = Arc::new(Runtime::embedded(ControlOptions::default()));
        desk.client = Some(client.clone());
        desk.status = Status::Attached;
        desk.link.tries = 4;
        for away in [None, Some(Duration::from_secs(1)), Some(REAPED_AFTER)] {
            desk.resume_link(away);
            assert_eq!(
                desk.link.retry_at, None,
                "the runtime redials, not a new client"
            );
        }
        assert_eq!(desk.link.tries, 0);
        assert!(
            desk.client
                .as_ref()
                .is_some_and(|c| Arc::ptr_eq(c, &client))
        );
    }

    #[test]
    fn the_phone_app_coming_to_the_front_resumes_its_link() {
        let configuration = serde_json::json!({"endpoint": "quic://localhost:8788"});
        let state = crate::mobile::state(serde_json::from_value(configuration).expect("parses"))
            .expect("a loopback host needs no pairing");
        let size = (393.0, 852.0);
        let mut rt = rune::Runtime::new(state, crate::ui::view, size, rune::Env::new(true, 2.0));
        rt.state.link.tries = 5;
        crate::mobile::set_active(&mut rt, false);
        assert!(!rt.state.focused);
        assert_eq!((rt.state.link.tries, rt.state.link.retry_at), (5, None));
        crate::mobile::set_active(&mut rt, true);
        assert!(rt.state.focused);
        // Nothing was scheduled, so a nudge alone would have waited.
        assert_eq!(rt.state.link.tries, 0);
        assert!(
            rt.state
                .link
                .retry_at
                .is_some_and(|at| at <= Instant::now())
        );
    }

    #[test]
    fn typing_probes_the_link_at_most_once_a_second() {
        let mut desk = desk();
        let client = Runtime::embedded(ControlOptions::default());
        desk.clock = 10.0;
        desk.nudge_typing(&client);
        assert_eq!(desk.link.nudged, Some(10.0));
        desk.clock = 10.9;
        desk.nudge_typing(&client);
        assert_eq!(desk.link.nudged, Some(10.0), "inside the second");
        desk.clock = 11.0;
        desk.nudge_typing(&client);
        assert_eq!(desk.link.nudged, Some(11.0));
        // A clock that started over can't silence the probe.
        desk.clock = 2.0;
        desk.nudge_typing(&client);
        assert_eq!(desk.link.nudged, Some(2.0));
    }

    #[test]
    fn a_key_the_connection_took_probes_it_and_a_paused_one_doesnt() {
        let (_client, mut rt) = crate::grid::budget::input_fixture();
        rt.frame(0.0);
        let type_a = |desk: &mut Desk| {
            desk.send_input(|control, terminal| control.send_text(terminal, "a"));
        };
        rt.update(|desk, _| {
            desk.clock = 7.0;
            type_a(desk);
        });
        assert_eq!(rt.state.link.nudged, Some(7.0));
        // A terminal that can't take input says so; there is no key to follow.
        rt.update(|desk, _| {
            desk.clock = 20.0;
            let shown = desk.shown.as_mut().expect("the fixture's terminal");
            shown.resource = phux_protocol::ids::ResourceId::local(9);
            type_a(desk);
        });
        assert_eq!(rt.state.link.nudged, Some(7.0));
        let notice = rt.state.notice.as_ref().map(|notice| notice.text.as_str());
        assert_eq!(notice, Some(crate::desk::input::INPUT_PAUSED));
    }
}
