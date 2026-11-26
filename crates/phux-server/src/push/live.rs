//! A closed phone's Lock Screen card
//! ([Live Activity](../../../../docs/protocol-files.md#live-activity)).
//!
//! A phone registers `phux.live/v1/<device>` like a push grant. While the
//! connection that wrote it is gone, the server posts how many agents wait
//! on the person and how many work whenever either count moves; the gateway
//! turns that into an `ActivityKit` push. Counts only: no pane, session or
//! question leaves the host.

use std::time::Duration;

use phux_protocol::wire::frame::{LIVE_GRANT_KEY_PREFIX, RESOURCE_AGENT_KEY};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::GrantValue;
use crate::state::{ServerState, SharedState};

// ponytail: polled like keep-awake, so the card lags by up to one TICK;
// hook the agent record's writers if that ever matters.
const TICK: Duration = Duration::from_secs(5);

/// Agents waiting on the person (blocked, or an action held for approval)
/// and agents working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) waiting: usize,
    pub(crate) working: usize,
}

pub(crate) fn spawn(state: &SharedState, root_token: &CancellationToken) {
    tokio::task::spawn_local(ticker(state.clone(), root_token.clone()));
}

async fn ticker(state: SharedState, token: CancellationToken) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = token.cancelled() => break,
            _ = tick.tick() => {}
        }
        for (target, counts) in state.with_mut(due) {
            tokio::spawn(async move {
                match post(&target, counts).await {
                    Ok(status) if (200..300).contains(&status) => {
                        debug!(host = %target.host, ?counts, "live: delivered to gateway");
                    }
                    Ok(status) => warn!(host = %target.host, status, "live: gateway refused"),
                    Err(err) => warn!(host = %target.host, %err, "live: send failed"),
                }
            });
        }
    }
}

/// The grants whose phone is away and whose card no longer shows the
/// counts, each with the counts to send; they are recorded as sent. A phone
/// still connected keeps its own card, so the first tick after it leaves
/// sends whatever is true then.
/// ponytail: a failed post is not retried; the next change resends.
pub(crate) fn due(s: &mut ServerState) -> Vec<(GrantValue, Counts)> {
    let mut now = None;
    let mut out = Vec::new();
    for (key, grant, live) in super::grants(s, LIVE_GRANT_KEY_PREFIX) {
        if live {
            s.push_grants_mut().synced.remove(&key);
            continue;
        }
        let counts = *now.get_or_insert_with(|| counts(s));
        if s.push_grants_mut().synced.insert(key, counts) != Some(counts) {
            out.push((grant, counts));
        }
    }
    out
}

fn counts(s: &ServerState) -> Counts {
    let (mut blocked, mut working) = (0, 0);
    for value in s.metadata().resource_values(RESOURCE_AGENT_KEY) {
        match phux_agent_session::state::stored_state(Some(value)).as_deref() {
            Some("blocked") => blocked += 1,
            Some("working") => working += 1,
            _ => {}
        }
    }
    Counts {
        waiting: blocked + s.pending_approval_count(),
        working,
    }
}

async fn post(target: &GrantValue, counts: Counts) -> Result<u16, String> {
    let body = serde_json::json!({
        "schema_version": 1,
        "host": target.host,
        "waiting": counts.waiting,
        "working": counts.working,
    });
    super::post_json(target, &body).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ClientId;
    use phux_protocol::wire::frame::Scope;

    #[test]
    fn an_away_phone_hears_each_change_once_and_a_present_one_nothing() {
        let mut s = ServerState::new();
        let key = format!("{LIVE_GRANT_KEY_PREFIX}phone-1");
        s.metadata_set(
            &Scope::Global,
            &key,
            br#"{"gateway":"https://live.example/live","grant":"0123456789abcdef","host":"mini"}"#
                .to_vec(),
        );
        assert!(s.push_grants_mut().register(&key, ClientId(7)));
        s.set_client_connection_cancellation(ClientId(7), CancellationToken::new());
        assert!(
            due(&mut s).is_empty(),
            "a connected phone keeps its own card"
        );

        s.forget_connection(ClientId(7));
        let quiet = Counts {
            waiting: 0,
            working: 0,
        };
        let sent: Vec<_> = due(&mut s).into_iter().map(|(g, c)| (g.host, c)).collect();
        assert_eq!(
            sent,
            [("mini".to_owned(), quiet)],
            "the first tick away syncs the card"
        );
        assert!(due(&mut s).is_empty(), "nothing moved");

        let pane = Scope::Resource(phux_protocol::ids::ResourceId::local(3));
        let record = br#"{"name":"rev","kind":"claude","state":"blocked"}"#.to_vec();
        s.metadata_set(&pane, RESOURCE_AGENT_KEY, record);
        let waits = Counts {
            waiting: 1,
            working: 0,
        };
        let sent: Vec<_> = due(&mut s).into_iter().map(|(_, c)| c).collect();
        assert_eq!(sent, [waits], "an agent that blocks moves the card");
    }
}
