//! phux-c2td.20: best-effort kills for satellite panes this client spawned
//! whose attach was then refused.
//!
//! A window or split spawned on a satellite (`new-window { host }`,
//! `split-pane` on a satellite pane) opens only once its pane attaches. When
//! that attach is refused the TUI opens nothing, but the satellite already
//! runs the pane, and nothing references it. The driver kills it through the
//! hub with the same host-qualified `KILL_RESOURCE` `phux kill host/@id`
//! sends. The satellite is often why the attach failed, so the kill can fail
//! too: its reply is consumed here and logged at debug, never surfaced.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind};

/// The orphan kills in flight, by request id.
#[derive(Debug, Default)]
pub(super) struct OrphanKills {
    in_flight: HashMap<u32, ResourceId>,
}

impl OrphanKills {
    /// One `KILL_RESOURCE` per orphaned pane, each under a fresh request id
    /// taken from `next_request_id` and tracked so its reply settles here.
    pub(super) fn kill_frames(
        &mut self,
        panes: Vec<ResourceId>,
        next_request_id: &mut u32,
    ) -> Vec<FrameKind> {
        panes
            .into_iter()
            .map(|pane| {
                let request_id = *next_request_id;
                *next_request_id = next_request_id.wrapping_add(1);
                self.in_flight.insert(request_id, pane.clone());
                FrameKind::Command {
                    request_id,
                    command: Command::KillResource { terminal_id: pane },
                }
            })
            .collect()
    }

    /// Consume the reply to one of these kills, logging how it went; hand
    /// any other frame back.
    pub(super) fn settle(&mut self, frame: FrameKind) -> Option<FrameKind> {
        let Some(pane) = reply_request_id(&frame).and_then(|id| self.in_flight.remove(&id)) else {
            return Some(frame);
        };
        log_kill_reply(&pane, &frame);
        None
    }
}

/// The request id a reply frame answers, if it is a reply.
const fn reply_request_id(frame: &FrameKind) -> Option<u32> {
    match frame {
        FrameKind::CommandResult { request_id, .. } => Some(*request_id),
        FrameKind::Error { request_id, .. } => *request_id,
        _ => None,
    }
}

/// Log an orphan kill's outcome. A failure is expected when the satellite
/// is unreachable, so it stays at debug.
fn log_kill_reply(pane: &ResourceId, frame: &FrameKind) {
    match frame {
        FrameKind::CommandResult {
            result: CommandResult::Ok,
            ..
        } => tracing::debug!(?pane, "killed the orphaned satellite pane"),
        FrameKind::CommandResult {
            result: CommandResult::Error { message, .. },
            ..
        }
        | FrameKind::Error { message, .. } => tracing::debug!(
            ?pane,
            %message,
            "could not kill the orphaned satellite pane; the satellite may be unreachable",
        ),
        _ => tracing::debug!(
            ?pane,
            "unexpected reply to an orphaned satellite pane's kill"
        ),
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::frame::ErrorCode;

    use super::*;

    fn edge(id: u32) -> ResourceId {
        ResourceId::satellite(SatelliteHost::new("edge"), id)
    }

    #[test]
    fn one_kill_per_orphan_under_fresh_request_ids() {
        let mut kills = OrphanKills::default();
        let mut next = 40;
        let frames = kills.kill_frames(vec![edge(9), edge(10)], &mut next);
        assert_eq!(
            frames,
            vec![
                FrameKind::Command {
                    request_id: 40,
                    command: Command::KillResource {
                        terminal_id: edge(9)
                    },
                },
                FrameKind::Command {
                    request_id: 41,
                    command: Command::KillResource {
                        terminal_id: edge(10)
                    },
                },
            ]
        );
        assert_eq!(next, 42);
    }

    #[test]
    fn a_kill_reply_is_consumed_once_whatever_it_says() {
        for reply in [
            FrameKind::CommandResult {
                request_id: 7,
                result: CommandResult::Ok,
            },
            FrameKind::CommandResult {
                request_id: 7,
                result: CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    message: "satellite edge link is down".to_owned(),
                },
            },
            FrameKind::Error {
                request_id: Some(7),
                code: ErrorCode::SatelliteUnreachable,
                message: "satellite edge link is down".to_owned(),
            },
        ] {
            let mut kills = OrphanKills::default();
            let mut next = 7;
            kills.kill_frames(vec![edge(9)], &mut next);
            assert_eq!(kills.settle(reply.clone()), None, "{reply:?}");
            assert_eq!(
                kills.settle(reply.clone()),
                Some(reply),
                "a second reply is not ours"
            );
        }
    }

    #[test]
    fn other_frames_pass_through() {
        let mut kills = OrphanKills::default();
        let mut next = 7;
        kills.kill_frames(vec![edge(9)], &mut next);
        let unrelated = FrameKind::CommandResult {
            request_id: 8,
            result: CommandResult::Ok,
        };
        assert_eq!(kills.settle(unrelated.clone()), Some(unrelated));
        let uncorrelated = FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message: "satellite edge is unreachable".to_owned(),
        };
        assert_eq!(kills.settle(uncorrelated.clone()), Some(uncorrelated));
    }
}
