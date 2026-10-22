use super::*;

/// What one connection's read loop does per `INPUT_PASTE` (ADR-0144):
/// take a credit, waiting while the pane is saturated, then route.
async fn type_into(
    state: SharedState,
    lane: InputLaneHandle,
    client: ClientId,
    pane: phux_protocol::ids::ResourceId,
    tag: char,
    count: usize,
) {
    let mut credits = InputCredits::default();
    for i in 0..count {
        let credit = credits
            .acquire(&state, &pane)
            .await
            .expect("the pane drains within the stall limit");
        let bytes = format!("{tag}{i:04};");
        lane.route(
            RoutedInput::attached(client, pane.clone(), paste(bytes.as_bytes()), "INPUT_PASTE")
                .with_credit(credit),
        )
        .await;
    }
}

/// Drain `count` writes, returning their concatenated bytes. Dropping
/// each request returns its credit, as the PTY writer does.
async fn drain(rx: &mut mpsc::Receiver<EncodedInputRequest>, count: usize) -> String {
    let mut out = String::new();
    for _ in 0..count {
        let request = next_write(rx).await;
        out.push_str(std::str::from_utf8(&request.bytes).expect("utf-8 payload"));
    }
    out
}

/// Every `{tag}{i};` a client typed, in the order it typed them.
fn typed_by(tag: char, count: usize) -> String {
    use std::fmt::Write as _;
    (0..count).fold(String::new(), |mut out, i| {
        let _ = write!(out, "{tag}{i:04};");
        out
    })
}

fn only(tag: char, written: &str) -> String {
    written
        .split_inclusive(';')
        .filter(|chunk| chunk.starts_with(tag))
        .collect()
}

/// ADR-0144: a pane whose writer has stalled saturates, and the clients
/// typing into it wait instead of losing input, while other panes and
/// clients keep flowing. Once the stalled writer drains, every event
/// arrives, in each client's order, and nothing deadlocks.
#[tokio::test(flavor = "current_thread")]
async fn a_saturated_pane_loses_no_input_and_blocks_no_other_pane() {
    const PER_CLIENT: usize = 3 * INPUT_CREDITS;
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let mut stalled = spawn_pane(&state, "a", b"");
            let mut others = [spawn_pane(&state, "b", b""), spawn_pane(&state, "c", b"")];
            let attach = |session: &str| {
                state.with_mut(|s| {
                    let id = s.new_client_id();
                    s.attach_default_caps(id, session, mpsc::channel(16).0)
                        .expect("attach");
                    id
                })
            };
            let (a1, a2, b, c) = (attach("a"), attach("a"), attach("b"), attach("c"));
            let lane = spawn_input_lane(state.clone()).expect("spawn lane");

            let mut typists = tokio::task::JoinSet::new();
            for (client, pane, tag) in [
                (a1, stalled.wire.clone(), 'x'),
                (a2, stalled.wire.clone(), 'y'),
                (b, others[0].wire.clone(), 'b'),
                (c, others[1].wire.clone(), 'c'),
            ] {
                typists.spawn_local(type_into(
                    state.clone(),
                    lane.handle(),
                    client,
                    pane,
                    tag,
                    PER_CLIENT,
                ));
            }

            // The other panes take everything while `a` is stuck.
            for (pane, tag) in others.iter_mut().zip(['b', 'c']) {
                let written = drain(&mut pane.writer_rx, PER_CLIENT).await;
                assert_eq!(written, typed_by(tag, PER_CLIENT), "pane {tag}");
            }

            // `a` holds exactly its credits, and its typists are waiting.
            tokio::time::timeout(LANE_DELIVERY_DEADLINE, async {
                while stalled.writer_rx.len() < INPUT_CREDITS {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the stalled pane fills its credits");
            assert_eq!(stalled.credits.available(), 0);
            assert_eq!(stalled.writer_rx.len(), INPUT_CREDITS);
            tokio::time::timeout(LANE_DELIVERY_DEADLINE, async {
                while typists.len() > 2 {
                    if typists.try_join_next().is_none() {
                        tokio::task::yield_now().await;
                    }
                }
            })
            .await
            .expect("the typists into `b` and `c` finish");
            assert!(
                typists.try_join_next().is_none(),
                "the typists into `a` wait for credits"
            );

            let written = drain(&mut stalled.writer_rx, 2 * PER_CLIENT).await;
            for tag in ['x', 'y'] {
                assert_eq!(
                    only(tag, &written),
                    typed_by(tag, PER_CLIENT),
                    "client {tag}"
                );
            }
            tokio::time::timeout(LANE_DELIVERY_DEADLINE, async {
                while typists.join_next().await.is_some() {}
            })
            .await
            .expect("every typist finishes");
            assert_no_write(&mut stalled.writer_rx).await;
            assert_eq!(stalled.credits.available(), INPUT_CREDITS);

            for pane in std::iter::once(&stalled).chain(&others) {
                pane.token.cancel();
            }
        })
        .await;
}
