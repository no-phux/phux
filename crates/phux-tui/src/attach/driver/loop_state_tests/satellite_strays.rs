/// The panes of every `KILL_RESOURCE` sent before a FIFO barrier.
async fn killed_panes(client: &mut Connection, server: &mut Connection) -> Vec<ResourceId> {
    kills_sent(client, server)
        .await
        .into_iter()
        .map(|(_, pane)| pane)
        .collect()
}

/// The next loop entry after a session switch that dropped a window still
/// waiting on the attach of `edge/@9`, the pane it spawned: that pane is a
/// stray carried into this loop.
async fn loop_with_switch_stray() -> (SessionLoop, Connection, Connection) {
    Box::pin(loop_with_switch_stray_on(ServerFeatureSet::new, None)).await
}

/// [`loop_with_switch_stray`] on a hub advertising `features`, the stray's
/// spawn bound to `instance` when that is `Some`.
async fn loop_with_switch_stray_on(
    features: fn() -> ServerFeatureSet,
    instance: Option<phux_protocol::ids::ServerInstance>,
) -> (SessionLoop, Connection, Connection) {
    let (mut state, _client, _server, _out) =
        Box::pin(loop_with_window_on(features(), instance)).await;
    let carried = state.orphans_for_switch();
    drop(state);
    let (mut next, client, server, _out) = Box::pin(bootstrapped_loop_with(features())).await;
    next.set_orphan_kills(carried);
    (next, client, server)
}

/// A refusal saying the satellite is unreachable records nothing: neither
/// a reachable inventory nor a spawn on that satellite later kills the pane,
/// since the satellite may have restarted and reused its id.
#[tokio::test(flavor = "current_thread")]
async fn an_unreachable_refusal_records_nothing() {
    use phux_protocol::wire::frame::ErrorCode;

    let (mut state, mut client, mut server, mut out) = Box::pin(loop_with_spawned_window()).await;
    Box::pin(refuse_spawned_attach(
        &mut state,
        &mut client,
        &mut out,
        ErrorCode::SatelliteUnreachable,
    ))
    .await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    Box::pin(answer_edge_spawn(&mut state, &mut client, 12)).await;
    assert_eq!(killed_panes(&mut client, &mut server).await, Vec::new());
}

/// A pane stranded by a session switch is killed, once, on the first reply
/// saying its satellite is reachable.
#[tokio::test(flavor = "current_thread")]
async fn a_switch_stray_is_killed_on_the_next_reachable_reply() {
    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(
        killed_panes(&mut client, &mut server).await,
        vec![spawned_edge_pane()]
    );

    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(
        killed_panes(&mut client, &mut server).await,
        Vec::new(),
        "the kill is sent once"
    );
}

/// A switch stray whose satellite answers only after
/// [`super::super::orphans::STRAY_TTL`] is forgotten, not killed.
#[tokio::test(flavor = "current_thread")]
async fn a_switch_stray_is_not_killed_after_its_ttl() {
    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
    state
        .orphan_kills
        .age_strays(super::super::orphans::STRAY_TTL + Duration::from_secs(1));
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(killed_panes(&mut client, &mut server).await, Vec::new());
}

/// An inventory that lists the stray's satellite as unreachable forgets it:
/// a later reachable one kills nothing.
#[tokio::test(flavor = "current_thread")]
async fn an_unreachable_inventory_row_drops_switch_strays() {
    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(false)]).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(killed_panes(&mut client, &mut server).await, Vec::new());
}

/// A stray this client has since adopted, in a window or in an open
/// waiting on its attach, is not killed when its satellite answers.
#[tokio::test(flavor = "current_thread")]
async fn an_adopted_stray_is_not_killed_on_retry() {
    use crate::attach::actions::Adopt;

    for adopted_by_window in [true, false] {
        let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
        if adopted_by_window {
            state
                .mirror
                .workspace
                .add_group("edge".to_owned(), spawned_edge_pane());
        } else {
            state.mirror.pending_windows.insert(
                901,
                PendingWindow {
                    name: "edge/build".to_owned(),
                    adopt: Some(Adopt::Existing(spawned_edge_pane())),
                },
            );
        }
        answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
        assert_eq!(
            killed_panes(&mut client, &mut server).await,
            Vec::new(),
            "adopted by a window: {adopted_by_window}"
        );
    }
}

/// Feed a satellite `new-window` spawn (request 901) its reply: `edge`
/// minted `edge/@{id}`.
async fn answer_edge_spawn(state: &mut SessionLoop, client: &mut Connection, id: u32) {
    use phux_protocol::wire::frame::SpawnResult;

    state.mirror.pending_windows.insert(
        901,
        PendingWindow {
            name: "3".to_owned(),
            adopt: None,
        },
    );
    let mut out = Vec::new();
    state
        .apply_server_frame(
            client,
            &mut out,
            None,
            FrameKind::ResourceSpawned {
                request_id: 901,
                result: SpawnResult::Ok(ResourceId::satellite(SatelliteHost::new("edge"), id)),
            },
            false,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
}

/// A satellite answering a relayed spawn is reachable, so its switch strays
/// are killed then too; one whose id that satellite has minted again (it
/// restarted) is forgotten instead.
#[tokio::test(flavor = "current_thread")]
async fn a_spawn_on_the_host_kills_its_strays_unless_it_restarted() {
    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
    Box::pin(answer_edge_spawn(&mut state, &mut client, 12)).await;
    assert_eq!(
        killed_panes(&mut client, &mut server).await,
        vec![spawned_edge_pane()]
    );

    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
    Box::pin(answer_edge_spawn(&mut state, &mut client, 5)).await;
    assert_eq!(killed_panes(&mut client, &mut server).await, Vec::new());
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(
        killed_panes(&mut client, &mut server).await,
        Vec::new(),
        "forgotten, not waiting"
    );
}

// ---- phux-c2td.25: conditional retries of stray satellite panes ----------

/// The instance token `edge` bound its spawns to.
fn edge_token() -> phux_protocol::ids::ServerInstance {
    phux_protocol::ids::ServerInstance::new([5; 16])
}

/// A hub that evaluates conditional kills.
fn conditional_kill() -> ServerFeatureSet {
    ServerFeatureSet::with(&[phux_protocol::caps::ServerFeature::ConditionalKill])
}

/// The conditional kill of `edge/@9` under [`edge_token`].
fn conditional_edge_kill() -> Command {
    Command::KillResourceIf {
        terminal_id: spawned_edge_pane(),
        precondition: phux_protocol::wire::frame::KillPrecondition::spawned_and_unattached(
            edge_token(),
        ),
        operation_id: None,
    }
}

/// The unconditional kill of `edge/@9`.
fn plain_edge_kill() -> Command {
    Command::KillResource {
        terminal_id: spawned_edge_pane(),
        operation_id: None,
    }
}

/// The kills sent before a FIFO barrier, without their request ids.
async fn kill_commands(client: &mut Connection, server: &mut Connection) -> Vec<Command> {
    kill_commands_sent(client, server)
        .await
        .into_iter()
        .map(|(_, command)| command)
        .collect()
}

/// A bound spawn stranded by an unreachable satellite is recorded, sends
/// nothing while the satellite is down, and is retried once with
/// `KILL_RESOURCE_IF` carrying its instance token once an inventory lists
/// the satellite reachable. The satellite's `PRECONDITION_FAILED` is
/// consumed and forgets the stray: no second attempt, and no unconditional
/// fallback.
#[tokio::test(flavor = "current_thread")]
async fn a_bound_unreachable_stray_is_retried_conditionally_once_reachable() {
    use phux_protocol::wire::frame::{CommandResult, ErrorCode};

    let (mut state, mut client, mut server, mut out) =
        Box::pin(loop_with_window_on(conditional_kill(), Some(edge_token()))).await;
    Box::pin(refuse_spawned_attach(
        &mut state,
        &mut client,
        &mut out,
        ErrorCode::SatelliteUnreachable,
    ))
    .await;
    assert_eq!(
        kill_commands(&mut client, &mut server).await,
        Vec::new(),
        "no kill while the satellite is unreachable"
    );

    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    let sent = kill_commands_sent(&mut client, &mut server).await;
    assert_eq!(
        sent.iter()
            .map(|(_, command)| command.clone())
            .collect::<Vec<_>>(),
        vec![conditional_edge_kill()]
    );
    let refused = FrameKind::CommandResult {
        request_id: sent[0].0,
        result: CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            message: "resource was attached by another connection".to_owned(),
        },
    };
    assert_eq!(state.orphan_kills.observe(refused), None, "consumed");

    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    Box::pin(answer_edge_spawn(&mut state, &mut client, 12)).await;
    assert_eq!(
        kill_commands(&mut client, &mut server).await,
        Vec::new(),
        "forgotten after its one retry"
    );
}

/// Without `CONDITIONAL_KILL` a bound spawn's unreachable refusal records
/// nothing, as before: neither a reachable inventory nor a spawn on the
/// satellite sends any kill, conditional or not.
#[tokio::test(flavor = "current_thread")]
async fn without_the_bit_an_unreachable_refusal_sends_no_kill_at_all() {
    use phux_protocol::wire::frame::ErrorCode;

    let (mut state, mut client, mut server, mut out) = Box::pin(loop_with_window_on(
        ServerFeatureSet::new(),
        Some(edge_token()),
    ))
    .await;
    Box::pin(refuse_spawned_attach(
        &mut state,
        &mut client,
        &mut out,
        ErrorCode::SatelliteUnreachable,
    ))
    .await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    Box::pin(answer_edge_spawn(&mut state, &mut client, 12)).await;
    assert_eq!(kill_commands(&mut client, &mut server).await, Vec::new());
}

/// A bound stray under the conditional kill outlives signs that its
/// satellite is unreachable (an unreachable inventory row, an uncorrelated
/// notice): the satellite judges the kill when it answers again.
#[tokio::test(flavor = "current_thread")]
async fn a_bound_stray_outlives_unreachable_signals() {
    use phux_protocol::wire::frame::ErrorCode;

    let (mut state, mut client, mut server, mut out) =
        Box::pin(loop_with_window_on(conditional_kill(), Some(edge_token()))).await;
    Box::pin(refuse_spawned_attach(
        &mut state,
        &mut client,
        &mut out,
        ErrorCode::SatelliteUnreachable,
    ))
    .await;
    answer_inventory(&mut state, &mut client, vec![edge_row(false)]).await;
    let notice = FrameKind::Error {
        request_id: None,
        code: ErrorCode::SatelliteUnreachable,
        message: "satellite edge is unreachable: link is down".to_owned(),
    };
    assert!(state.orphan_kills.observe(notice).is_some());
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(
        kill_commands(&mut client, &mut server).await,
        vec![conditional_edge_kill()]
    );
}

/// A session-switch stray whose spawn was bound is retried with the
/// conditional kill on a hub with the bit, and with today's unconditional
/// kill on one without it.
#[tokio::test(flavor = "current_thread")]
async fn a_bound_switch_stray_uses_the_conditional_kill_when_supported() {
    let cases: [(fn() -> ServerFeatureSet, Command); 2] = [
        (conditional_kill, conditional_edge_kill()),
        (ServerFeatureSet::new, plain_edge_kill()),
    ];
    for (features, expected) in cases {
        let (mut state, mut client, mut server) =
            Box::pin(loop_with_switch_stray_on(features, Some(edge_token()))).await;
        answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
        assert_eq!(
            kill_commands(&mut client, &mut server).await,
            vec![expected]
        );
    }
}

/// An unbound switch stray keeps today's behavior on a hub with the bit:
/// an unconditional kill on the next reachable reply, and forgotten by an
/// unreachable inventory row first.
#[tokio::test(flavor = "current_thread")]
async fn an_unbound_switch_stray_is_unchanged_under_the_bit() {
    let (mut state, mut client, mut server) =
        Box::pin(loop_with_switch_stray_on(conditional_kill, None)).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(
        kill_commands(&mut client, &mut server).await,
        vec![plain_edge_kill()]
    );

    let (mut state, mut client, mut server) =
        Box::pin(loop_with_switch_stray_on(conditional_kill, None)).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(false)]).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(kill_commands(&mut client, &mut server).await, Vec::new());
}

/// A bound stray this client has since adopted is not killed, even
/// conditionally: the satellite exempts this client's own attaches.
#[tokio::test(flavor = "current_thread")]
async fn an_adopted_bound_stray_is_not_killed_conditionally() {
    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray_on(
        conditional_kill,
        Some(edge_token()),
    ))
    .await;
    state
        .mirror
        .workspace
        .add_group("edge".to_owned(), spawned_edge_pane());
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(kill_commands(&mut client, &mut server).await, Vec::new());
}

// ---- phux-c2td.3: held federation notices --------------------------------

fn host_rows() -> Vec<phux_protocol::wire::info::HostInventory> {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::info::HostInventory;

    vec![
        HostInventory::unreachable(
            SatelliteHost::new("down"),
            "satellite down is unreachable: link is down",
        ),
        HostInventory::reachable(SatelliteHost::new("edge"), Vec::new()),
    ]
}

/// The reply drops only the notices its unreachable rows explain; a link
/// drop for a satellite it still lists as reachable surfaces.
#[test]
fn held_notices_surface_unless_the_inventory_explains_them() {
    let held = vec![
        // Explained: the row carries exactly this diagnostic.
        "satellite down is unreachable: link is down".to_owned(),
        // Explained: same host, a different reason.
        "satellite down is unreachable: dial refused".to_owned(),
        // Not explained: the inventory lists `edge` as reachable.
        "satellite edge is unreachable: link dropped".to_owned(),
        // Not explained: a different host whose name merely extends `down`.
        "satellite downstairs is unreachable: gone".to_owned(),
    ];
    assert_eq!(
        unexplained_unreachable_notices(held, &host_rows()),
        vec![
            "satellite edge is unreachable: link dropped".to_owned(),
            "satellite downstairs is unreachable: gone".to_owned(),
        ]
    );
}

// ---- phux-4s6o: session rename confirmation and peer identity ------------
