use phux_protocol::ids::{SatelliteHost, ServerInstance};
use phux_protocol::wire::frame::{ErrorCode, KillPrecondition, SpawnError, SpawnResult};

use super::*;

fn edge(id: u32) -> ResourceId {
    ResourceId::satellite(SatelliteHost::new("edge"), id)
}

fn other(id: u32) -> ResourceId {
    ResourceId::satellite(SatelliteHost::new("other"), id)
}

fn hosts(names: &[&str]) -> Vec<SatelliteHost> {
    names.iter().map(|name| SatelliteHost::new(*name)).collect()
}

fn token() -> ServerInstance {
    ServerInstance::new([7; 16])
}

fn bound(pane: ResourceId) -> SpawnedPane {
    SpawnedPane {
        id: pane,
        instance: Some(token()),
    }
}

fn unreachable_reply(request_id: u32, host: &str) -> FrameKind {
    FrameKind::Error {
        request_id: Some(request_id),
        code: ErrorCode::SatelliteUnreachable,
        message: format!("satellite {host} link is down"),
    }
}

/// Strays recorded as a switch would, with no spawns still unanswered.
fn with_strays(panes: Vec<ResourceId>, now: Instant) -> OrphanKills {
    with_spawned(
        panes.into_iter().map(SpawnedPane::unbound).collect(),
        false,
        now,
    )
}

/// [`with_strays`] for spawned panes, on a hub with or without
/// `CONDITIONAL_KILL`.
fn with_spawned(panes: Vec<SpawnedPane>, conditional_kill: bool, now: Instant) -> OrphanKills {
    let mut kills = OrphanKills::default();
    kills.set_conditional_kill(conditional_kill);
    kills.park_for_switch(panes, HashSet::new(), now);
    kills
}

/// The panes of `strays`, in order.
fn panes_of(strays: Vec<Stray>) -> Vec<ResourceId> {
    strays.into_iter().map(|stray| stray.pane).collect()
}

/// Every stray still waiting, whatever its host.
fn waiting(kills: &mut OrphanKills, now: Instant) -> Vec<ResourceId> {
    let mut due = panes_of(kills.take_answered(&hosts(&["edge", "other"]), now, now));
    due.sort();
    due
}

/// The retry each stray on `edge` would get now.
fn retries(kills: &mut OrphanKills, now: Instant) -> Vec<FrameKind> {
    let due = kills.take_answered(&hosts(&["edge"]), now, now);
    let mut next = 1;
    kills.stray_kill_frames(due, &mut next)
}

/// The request id and command of the one retry `frames` holds.
fn only_command(frames: Vec<FrameKind>) -> (u32, Command) {
    let [
        FrameKind::Command {
            request_id,
            command,
        },
    ] = <[FrameKind; 1]>::try_from(frames).expect("exactly one retry")
    else {
        panic!("expected a command frame");
    };
    (request_id, command)
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
        unreachable_reply(7, "edge"),
    ] {
        let mut kills = OrphanKills::default();
        let mut next = 7;
        kills.kill_frames(vec![edge(9)], &mut next);
        assert_eq!(kills.observe(reply.clone()), None, "{reply:?}");
        assert_eq!(
            kills.observe(reply.clone()),
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
    assert_eq!(kills.observe(unrelated.clone()), Some(unrelated));
    let uncorrelated = FrameKind::Error {
        request_id: None,
        code: ErrorCode::SatelliteUnreachable,
        message: "satellite edge is unreachable".to_owned(),
    };
    assert_eq!(kills.observe(uncorrelated.clone()), Some(uncorrelated));
}

/// phux-c2td.23: a stray is handed back once its satellite answers, and
/// only its satellite: another one answering says nothing about it.
#[test]
fn a_stray_is_due_once_its_host_answers() {
    let recorded = Instant::now();
    let mut kills = with_strays(vec![edge(9)], recorded);
    assert!(
        kills
            .take_answered(&hosts(&["other"]), recorded, recorded)
            .is_empty()
    );
    assert_eq!(
        panes_of(kills.take_answered(&hosts(&["edge"]), recorded, recorded)),
        vec![edge(9)]
    );
    assert!(
        kills
            .take_answered(&hosts(&["edge"]), recorded, recorded)
            .is_empty(),
        "handed back once"
    );
}

/// An answer from before the stray was recorded is not news about it.
#[test]
fn an_answer_older_than_the_stray_does_not_retry_it() {
    let asked = Instant::now();
    let recorded = asked + Duration::from_secs(1);
    let mut kills = with_strays(vec![edge(9)], recorded);
    assert!(
        kills
            .take_answered(&hosts(&["edge"]), asked, recorded)
            .is_empty()
    );
    assert_eq!(
        panes_of(kills.take_answered(&hosts(&["edge"]), recorded, recorded)),
        vec![edge(9)]
    );
}

/// The record keeps at most [`STRAY_CAP`] panes, dropping the oldest,
/// and never holds one pane twice.
#[test]
fn the_record_is_capped_oldest_first_and_deduplicated() {
    let now = Instant::now();
    let cap = u32::try_from(STRAY_CAP).unwrap();
    let mut kills = with_strays((1..=cap + 2).map(edge).collect(), now);
    kills.park_for_switch(
        vec![SpawnedPane::unbound(edge(cap + 2))],
        HashSet::new(),
        now,
    );
    let due = panes_of(kills.take_answered(&hosts(&["edge"]), now, now));
    assert_eq!(due, (3..=cap + 2).map(edge).collect::<Vec<_>>());
}

/// The cap holds across both kinds: unreachable strays share the record.
#[test]
fn unreachable_strays_share_the_cap() {
    let now = Instant::now();
    let cap = u32::try_from(STRAY_CAP).unwrap();
    let mut kills = with_spawned(Vec::new(), true, now);
    kills.record_unreachable(
        (1..=cap + 1)
            .map(|id| bound(edge(id)).bound().unwrap())
            .collect(),
        now,
    );
    let due = panes_of(kills.take_answered(&hosts(&["edge"]), now, now));
    assert_eq!(due, (2..=cap + 1).map(edge).collect::<Vec<_>>());
}

/// An unconditional stray is still killed when its satellite answers
/// within [`STRAY_TTL`], and forgotten when the answer comes any later.
#[test]
fn a_stray_is_due_within_its_ttl_and_forgotten_after() {
    let recorded = Instant::now();
    let within = recorded + STRAY_TTL;
    let mut kills = with_strays(vec![edge(9)], recorded);
    assert_eq!(
        panes_of(kills.take_answered(&hosts(&["edge"]), within, within)),
        vec![edge(9)]
    );

    let past = within + Duration::from_secs(1);
    let mut kills = with_strays(vec![edge(9)], recorded);
    assert!(
        kills
            .take_answered(&hosts(&["edge"]), past, past)
            .is_empty()
    );
}

/// phux-c2td.25: a conditional stray outlives [`STRAY_TTL`] and is
/// forgotten only past [`BOUND_STRAY_TTL`].
#[test]
fn a_conditional_stray_waits_for_the_longer_ttl() {
    let recorded = Instant::now();
    let past_short = recorded + STRAY_TTL + Duration::from_secs(1);
    let mut kills = with_spawned(vec![bound(edge(9))], true, recorded);
    assert_eq!(
        panes_of(kills.take_answered(&hosts(&["edge"]), past_short, past_short)),
        vec![edge(9)]
    );

    let past_long = recorded + BOUND_STRAY_TTL + Duration::from_secs(1);
    let mut kills = with_spawned(vec![bound(edge(9))], true, recorded);
    assert!(
        kills
            .take_answered(&hosts(&["edge"]), past_long, past_long)
            .is_empty()
    );
}

/// A kill is one attempt: a failed one is never recorded, so a stray
/// whose kill fails is dropped after that one retry.
#[test]
fn a_failed_kill_is_not_remembered() {
    let now = Instant::now();
    let mut kills = with_strays(vec![edge(9)], now);
    let due = kills.take_answered(&hosts(&["edge"]), now, now);
    let mut next = 1;
    kills.stray_kill_frames(due, &mut next);
    kills.kill_frames(vec![other(4)], &mut next);
    let busy = FrameKind::Error {
        request_id: Some(1),
        code: ErrorCode::ResourceExhausted,
        message: "satellite edge link is saturated; retry".to_owned(),
    };
    assert_eq!(kills.observe(busy), None);
    assert_eq!(kills.observe(unreachable_reply(2, "other")), None);
    assert_eq!(waiting(&mut kills, now), Vec::new());
}

/// Any frame saying a satellite is unreachable forgets that satellite's
/// unconditional strays and keeps the rest: an uncorrelated notice, a
/// refused command, a refused spawn. A message naming no satellite forgets
/// them all; a failure for another reason forgets nothing.
#[test]
fn an_unreachable_satellite_forgets_its_strays() {
    let now = Instant::now();
    let edge_down = [
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message: "satellite edge is unreachable: link is down".to_owned(),
        },
        FrameKind::CommandResult {
            request_id: 3,
            result: CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: "satellite edge did not answer within 30s".to_owned(),
            },
        },
        FrameKind::ResourceSpawned {
            request_id: 3,
            result: SpawnResult::Err(SpawnError::SatelliteUnreachable(
                "satellite edge link is down".to_owned(),
            )),
        },
    ];
    for frame in edge_down {
        let mut kills = with_strays(vec![edge(9), other(4)], now);
        kills.observe(frame.clone());
        assert_eq!(waiting(&mut kills, now), vec![other(4)], "{frame:?}");
    }

    let mut kills = with_strays(vec![edge(9), other(4)], now);
    kills.observe(FrameKind::Error {
        request_id: None,
        code: ErrorCode::SatelliteUnreachable,
        message: "unreachable".to_owned(),
    });
    assert_eq!(waiting(&mut kills, now), Vec::new(), "no host named");

    let mut kills = with_strays(vec![edge(9)], now);
    kills.observe(FrameKind::CommandResult {
        request_id: 3,
        result: CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "satellite edge has no such terminal".to_owned(),
        },
    });
    assert_eq!(waiting(&mut kills, now), vec![edge(9)], "not unreachable");
}

/// A host inventory's unreachable rows forget those satellites' strays.
#[test]
fn unreachable_hosts_forget_their_strays() {
    let now = Instant::now();
    let mut kills = with_strays(vec![edge(9), other(4)], now);
    kills.forget_hosts(&hosts(&["edge"]));
    assert_eq!(waiting(&mut kills, now), vec![other(4)]);
}

/// phux-c2td.25: a conditional stray outlives every unreachable signal,
/// since the satellite judges its kill; an unconditional one beside it on
/// the same host is forgotten.
#[test]
fn a_conditional_stray_outlives_unreachable_signals() {
    let now = Instant::now();
    let mut kills = with_spawned(
        vec![bound(edge(9)), SpawnedPane::unbound(edge(10))],
        true,
        now,
    );
    kills.observe(unreachable_reply(3, "edge"));
    kills.observe(FrameKind::Error {
        request_id: None,
        code: ErrorCode::SatelliteUnreachable,
        message: "unreachable".to_owned(),
    });
    kills.forget_hosts(&hosts(&["edge"]));
    assert_eq!(waiting(&mut kills, now), vec![edge(9)]);
}

/// A pane freshly minted on the same host with an id no greater than a
/// stray's means that satellite restarted: the stray is forgotten, and a
/// stray on another host, or with a lower id, is kept.
#[test]
fn a_reissued_id_forgets_the_stray() {
    let now = Instant::now();
    let mut kills = with_strays(vec![edge(3), edge(9), other(9)], now);
    kills.forget_reissued(&[edge(5)]);
    assert_eq!(waiting(&mut kills, now), vec![edge(3), other(9)]);
}

/// A session switch remembers the parked panes and the satellite panes
/// its unanswered spawns turn out to be; the drain settles kill replies,
/// forgets a satellite it shows unreachable, and clears the old
/// request-id space.
#[test]
fn a_switch_remembers_parked_and_drained_spawns() {
    let mut kills = OrphanKills::default();
    let mut next = 50;
    kills.kill_frames(vec![other(1)], &mut next);
    let now = Instant::now();
    kills.park_for_switch(
        vec![
            SpawnedPane::unbound(edge(9)),
            SpawnedPane::unbound(other(2)),
        ],
        HashSet::from([20, 21, 22]),
        now,
    );
    for frame in [
        FrameKind::ResourceSpawned {
            request_id: 20,
            result: SpawnResult::Ok(edge(10)),
        },
        FrameKind::ResourceSpawned {
            request_id: 21,
            result: SpawnResult::Ok(ResourceId::local(4)),
        },
        FrameKind::ResourceSpawned {
            request_id: 22,
            result: SpawnResult::Err(SpawnError::SpawnFailed("no shell".to_owned())),
        },
        FrameKind::ResourceSpawned {
            request_id: 23,
            result: SpawnResult::Ok(edge(11)),
        },
        unreachable_reply(50, "other"),
    ] {
        kills.observe_switch_drain(frame, now);
    }
    kills.finish_switch_drain();
    assert!(kills.in_flight.is_empty() && kills.switch_spawns.is_empty());
    assert_eq!(waiting(&mut kills, now), vec![edge(9), edge(10)]);
}

/// phux-c2td.25: a bound switch stray is retried with `KILL_RESOURCE_IF`
/// carrying its instance token and the attachment condition when the hub
/// advertises `CONDITIONAL_KILL`, and with today's `KILL_RESOURCE` when it
/// does not. An unbound one keeps `KILL_RESOURCE` either way.
#[test]
fn a_switch_stray_is_retried_conditionally_only_when_bound_and_supported() {
    let now = Instant::now();
    let conditional = FrameKind::Command {
        request_id: 1,
        command: Command::KillResourceIf {
            terminal_id: edge(9),
            precondition: KillPrecondition::spawned_and_unattached(token()),
        },
    };
    let plain = FrameKind::Command {
        request_id: 1,
        command: Command::KillResource {
            terminal_id: edge(9),
        },
    };
    for (pane, supported, expected) in [
        (bound(edge(9)), true, conditional),
        (bound(edge(9)), false, plain.clone()),
        (SpawnedPane::unbound(edge(9)), true, plain),
    ] {
        let mut kills = with_spawned(vec![pane.clone()], supported, now);
        assert_eq!(
            retries(&mut kills, now),
            vec![expected],
            "{pane:?}, supported: {supported}"
        );
    }
}

/// A spawn the switch drained that answers bound is remembered with its
/// token and retried conditionally.
#[test]
fn a_drained_bound_spawn_is_retried_conditionally() {
    let now = Instant::now();
    let mut kills = OrphanKills::default();
    kills.set_conditional_kill(true);
    kills.park_for_switch(Vec::new(), HashSet::from([20]), now);
    kills.observe_switch_drain(
        FrameKind::ResourceSpawned {
            request_id: 20,
            result: SpawnResult::OkBound {
                id: edge(9),
                instance: token(),
            },
        },
        now,
    );
    kills.finish_switch_drain();
    let (_, command) = only_command(retries(&mut kills, now));
    assert_eq!(command, bound(edge(9)).bound().unwrap().kill_command());
}

/// phux-c2td.25: a pane an unreachable satellite stranded is recorded for
/// a conditional retry only when the hub advertises `CONDITIONAL_KILL`;
/// without it nothing is recorded, so neither kill is ever sent.
#[test]
fn unreachable_strays_are_recorded_only_with_the_bit() {
    let now = Instant::now();
    let stranded = || vec![bound(edge(9)).bound().unwrap()];

    let mut kills = with_spawned(Vec::new(), false, now);
    kills.record_unreachable(stranded(), now);
    assert_eq!(retries(&mut kills, now), Vec::new(), "no bit, no record");

    let mut kills = with_spawned(Vec::new(), true, now);
    kills.record_unreachable(stranded(), now);
    let (_, command) = only_command(retries(&mut kills, now));
    assert_eq!(command, stranded()[0].kill_command());
}

/// A conditional kill's refusal, whatever it says, is consumed and the
/// stray is not remembered: no second attempt, and never an unconditional
/// one.
#[test]
fn a_refused_conditional_kill_forgets_the_stray() {
    for code in [
        ErrorCode::PreconditionFailed,
        ErrorCode::TerminalNotFound,
        ErrorCode::SatelliteUnreachable,
    ] {
        let now = Instant::now();
        let mut kills = with_spawned(vec![bound(edge(9))], true, now);
        let (request_id, _) = only_command(retries(&mut kills, now));
        let refusal = FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code,
                message: "refused".to_owned(),
            },
        };
        assert_eq!(kills.observe(refusal), None, "{code:?}");
        assert_eq!(waiting(&mut kills, now), Vec::new(), "{code:?}");
    }
}
