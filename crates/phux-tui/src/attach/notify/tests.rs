use super::*;

fn cfg() -> NotificationsCfg {
    NotificationsCfg::default()
}

fn row(id: u32, phase: Phase) -> NotifyRow {
    NotifyRow {
        key: ResourceId::local(id),
        name: "claude".to_owned(),
        phase,
        asking: false,
        session: "dev".to_owned(),
        place: "api".to_owned(),
        host: None,
        viewing: false,
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "reads as the `prior` argument at every call site"
)]
fn track(phase: Phase, worked: bool) -> Option<Track> {
    Some(Track {
        phase,
        worked,
        viewing: false,
    })
}

/// `decide` for pane 1 in `phase`, the client window focused.
fn step(prior: Option<Track>, phase: Phase, primed: bool) -> (Track, Option<NotifyKind>) {
    decide(prior, &row(1, phase), true, primed, cfg())
}

/// Single transitions: working to blocked fires (and the run still
/// finishes later); staying blocked does not refire; settling without
/// work is silent; a first sighting of a blocked pane fires only once
/// the notifier is primed.
#[test]
fn single_transitions_fire_only_on_a_new_edge() {
    let blocked = Some(NotifyKind::Blocked { asking: false });
    #[rustfmt::skip] // a case table, one row per case
    let cases = [
        ("working to blocked", track(Phase::Working, true), Phase::Blocked, true, blocked),
        ("staying blocked", track(Phase::Blocked, true), Phase::Blocked, true, None),
        ("idle without work", None, Phase::Settled, true, None),
        ("unknown without work", track(Phase::Unknown, false), Phase::Settled, true, None),
        ("first sighting, unprimed", None, Phase::Blocked, false, None),
        ("first sighting, primed", None, Phase::Blocked, true, blocked),
    ];
    for (case, prior, phase, primed, want) in cases {
        assert_eq!(step(prior, phase, primed).1, want, "{case}");
    }
    let (next, _) = step(track(Phase::Working, true), Phase::Blocked, true);
    assert!(next.worked, "a blocked run still finishes later");
}

#[test]
fn done_fires_once_per_run() {
    let (next, edge) = step(track(Phase::Working, true), Phase::Settled, true);
    assert_eq!(edge, Some(NotifyKind::Done));
    assert!(!next.worked);
    let (_, again) = step(Some(next), Phase::Settled, true);
    assert_eq!(again, None, "a second identical done is not a new run");
}

#[test]
fn ask_names_itself() {
    let mut asked = row(1, Phase::Blocked);
    asked.asking = true;
    let (_, edge) = decide(track(Phase::Working, true), &asked, true, true, cfg());
    assert_eq!(edge, Some(NotifyKind::Blocked { asking: true }));
}

#[test]
fn focused_pane_in_focused_window_is_suppressed() {
    let mut viewing = row(1, Phase::Settled);
    viewing.viewing = true;
    let prior = track(Phase::Working, true);
    assert_eq!(decide(prior, &viewing, true, true, cfg()).1, None);
    // The terminal window lost focus: the user is not looking.
    assert_eq!(
        decide(prior, &viewing, false, true, cfg()).1,
        Some(NotifyKind::Done)
    );
    let loud = NotificationsCfg {
        suppress_focused: false,
        ..cfg()
    };
    assert_eq!(
        decide(prior, &viewing, true, true, loud).1,
        Some(NotifyKind::Done)
    );
}

#[test]
fn mode_off_and_kind_toggles_silence_but_still_track() {
    let working = || track(Phase::Working, true);
    let off = NotificationsCfg {
        mode: NotifyMode::Off,
        ..cfg()
    };
    let (next, edge) = decide(working(), &row(1, Phase::Settled), true, true, off);
    assert_eq!(edge, None);
    assert!(!next.worked, "the run is consumed even when silent");
    let no_done = NotificationsCfg {
        done: false,
        ..cfg()
    };
    assert_eq!(
        decide(working(), &row(1, Phase::Settled), true, true, no_done).1,
        None
    );
    let no_blocked = NotificationsCfg {
        blocked: false,
        ..cfg()
    };
    assert_eq!(
        decide(working(), &row(1, Phase::Blocked), true, true, no_blocked).1,
        None
    );
}

#[test]
fn phase_folds_attention_over_state() {
    assert_eq!(Phase::of(AgentMetaState::Idle, true), Phase::Blocked);
    assert_eq!(Phase::of(AgentMetaState::Done, false), Phase::Settled);
    assert_eq!(Phase::of(AgentMetaState::Idle, false), Phase::Settled);
    assert_eq!(Phase::of(AgentMetaState::Working, false), Phase::Working);
    assert_eq!(Phase::of(AgentMetaState::Unknown, false), Phase::Unknown);
}

#[test]
fn describe_names_agent_place_session_and_host() {
    let mut r = row(1, Phase::Blocked);
    r.host = Some("buildbox".to_owned());
    let note = describe(&r, NotifyKind::Blocked { asking: false });
    assert_eq!(note.title, "claude needs you");
    assert_eq!(note.body, "api in dev on buildbox");
    let done = describe(&row(1, Phase::Settled), NotifyKind::Done);
    assert_eq!(done.title, "claude done");
    assert_eq!(done.body, "api in dev");
}

/// Prime a notifier and walk one pane to working.
fn primed_working(now: Instant) -> Notifier {
    let mut n = Notifier::new(now);
    n.observe(&[row(1, Phase::Working)], cfg(), now + ATTACH_GRACE);
    n
}

#[test]
fn edge_waits_out_the_coalesce_window() {
    let t0 = Instant::now();
    let mut n = primed_working(t0);
    let t1 = t0 + ATTACH_GRACE;
    n.observe(&[row(1, Phase::Settled)], cfg(), t1);
    assert_eq!(n.deadline(), Some(t1 + COALESCE));
    assert_eq!(n.take_due(cfg(), t1), None, "not due yet");
    let note = n.take_due(cfg(), t1 + COALESCE).expect("due");
    assert_eq!(note.title, "claude done");
    assert_eq!(n.deadline(), None);
}

#[test]
fn a_flap_back_inside_the_window_cancels() {
    let t0 = Instant::now();
    let mut n = primed_working(t0);
    let t1 = t0 + ATTACH_GRACE;
    n.observe(&[row(1, Phase::Settled)], cfg(), t1);
    n.observe(&[row(1, Phase::Working)], cfg(), t1 + COALESCE / 2);
    assert_eq!(n.take_due(cfg(), t1 + COALESCE), None);
}

#[test]
fn viewing_the_pane_before_the_post_cancels_it() {
    let t0 = Instant::now();
    let mut n = primed_working(t0);
    let t1 = t0 + ATTACH_GRACE;
    n.observe(&[row(1, Phase::Settled)], cfg(), t1);
    let mut looked = row(1, Phase::Settled);
    looked.viewing = true;
    n.observe(&[looked], cfg(), t1 + COALESCE / 2);
    assert_eq!(n.take_due(cfg(), t1 + COALESCE), None);
}

#[test]
fn a_burst_posts_as_one_summary() {
    let t0 = Instant::now();
    let mut n = Notifier::new(t0);
    let t1 = t0 + ATTACH_GRACE;
    n.observe(&[row(1, Phase::Working), row(2, Phase::Working)], cfg(), t1);
    let mut other = row(2, Phase::Blocked);
    other.name = "codex".to_owned();
    n.observe(&[row(1, Phase::Settled), other], cfg(), t1);
    let note = n.take_due(cfg(), t1 + COALESCE).expect("due");
    assert_eq!(note.title, "2 need you");
    assert_eq!(note.body, "claude done, codex needs you");
}

#[test]
fn a_flapping_agent_is_rate_limited() {
    let t0 = Instant::now();
    let mut n = primed_working(t0);
    let mut t = t0 + ATTACH_GRACE;
    n.observe(&[row(1, Phase::Settled)], cfg(), t);
    t += COALESCE;
    assert!(n.take_due(cfg(), t).is_some());
    n.observe(&[row(1, Phase::Working)], cfg(), t);
    n.observe(&[row(1, Phase::Settled)], cfg(), t);
    t += COALESCE;
    assert_eq!(n.take_due(cfg(), t), None, "inside the rate limit");
    n.observe(&[row(1, Phase::Working)], cfg(), t);
    t += RATE_LIMIT;
    n.observe(&[row(1, Phase::Settled)], cfg(), t);
    assert!(n.take_due(cfg(), t + COALESCE).is_some());
}

#[test]
fn attach_grace_baselines_existing_blocked_agents() {
    let t0 = Instant::now();
    let mut n = Notifier::new(t0);
    n.observe(&[row(1, Phase::Blocked)], cfg(), t0);
    assert_eq!(n.deadline(), None);
    // A new blocked pane after the grace does fire.
    let late = t0 + ATTACH_GRACE;
    n.observe(
        &[row(1, Phase::Blocked), row(2, Phase::Blocked)],
        cfg(),
        late,
    );
    let note = n.take_due(cfg(), late + COALESCE).expect("due");
    assert_eq!(note.title, "claude needs you");
}

#[test]
fn losing_window_focus_lets_the_focused_pane_notify() {
    let t0 = Instant::now();
    let mut n = Notifier::new(t0);
    let t1 = t0 + ATTACH_GRACE;
    let mut working = row(1, Phase::Working);
    working.viewing = true;
    n.observe(std::slice::from_ref(&working), cfg(), t1);
    n.set_client_focused(false);
    let mut done = row(1, Phase::Settled);
    done.viewing = true;
    n.observe(&[done], cfg(), t1);
    assert!(n.take_due(cfg(), t1 + COALESCE).is_some());
}

#[test]
fn a_muted_pane_never_posts_and_unmuting_restores_it() {
    let t0 = Instant::now();
    let mut n = primed_working(t0);
    let t1 = t0 + ATTACH_GRACE;
    n.observe(&[row(1, Phase::Settled)], cfg(), t1);
    // Muting drops what was already queued, and nothing new queues.
    n.set_mode(ResourceId::local(1), PaneNotify::UnreadOnly);
    assert_eq!(n.take_due(cfg(), t1 + COALESCE), None);
    n.observe(&[row(1, Phase::Working)], cfg(), t1);
    n.observe(&[row(1, Phase::Blocked)], cfg(), t1);
    assert_eq!(n.deadline(), None);
    n.set_mode(ResourceId::local(1), PaneNotify::On);
    n.observe(&[row(1, Phase::Settled)], cfg(), t1);
    assert!(n.take_due(cfg(), t1 + COALESCE).is_some());
}

#[test]
fn a_note_notifies_out_of_sight_once_per_rate_limit() {
    let now = Instant::now();
    let pane = ResourceId::local(3);
    let mut notifier = Notifier::new(now);
    assert!(!notifier.note_due(&pane, true, cfg(), now), "on screen");
    assert!(notifier.note_due(&pane, false, cfg(), now));
    assert!(!notifier.note_due(&pane, false, cfg(), now + COALESCE));
    assert!(notifier.note_due(&pane, false, cfg(), now + RATE_LIMIT));
    notifier.set_client_focused(false);
    let other = ResourceId::local(4);
    assert!(
        notifier.note_due(&other, true, cfg(), now),
        "window unfocused"
    );
    let off = NotificationsCfg {
        mode: NotifyMode::Off,
        ..cfg()
    };
    assert!(!notifier.note_due(&ResourceId::local(5), false, off, now));
}
