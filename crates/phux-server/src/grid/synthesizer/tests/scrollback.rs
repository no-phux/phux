use super::*;

/// A scrollback-bearing snapshot applied as the client does reconstructs
/// both the viewport and every history row.
#[test]
fn scrollback_snapshot_round_trips_history_and_viewport() {
    // 4-row grid; 10 numbered lines push 6 into history (10 - 4 visible).
    let mut source = fresh(20, 4);
    for i in 1..=10 {
        source.vt_write(format!("line{i}\r\n").as_bytes());
    }
    // After the trailing CRLF the cursor sits on a fresh blank row, so the
    // viewport is [line8, line9, line10, ""] and history holds line1..=7.
    let synth = SnapshotSynthesizer::new().expect("synth");
    let snap = synth
        .synthesize_with_scrollback(&source, Some(SCROLLBACK_ALL))
        .expect("synthesize_with_scrollback");
    assert!(
        !snap.scrollback.is_empty(),
        "history present ⇒ scrollback-priming bytes emitted"
    );

    // Replay onto a fresh client terminal, wire order: scrollback, viewport.
    let mut client = fresh(20, 4);
    client.vt_write(&snap.scrollback);
    client.vt_write(&snap.bytes);

    // Viewport matches.
    assert_eq!(render_grid(&client), render_grid(&source));

    // History matches, row for row, with nothing dropped.
    let mut sb_synth = SnapshotSynthesizer::new().expect("synth2");
    let source_hist = sb_synth
        .screen_state_with_scrollback(&source, 0, Some(SCROLLBACK_ALL), false)
        .expect("source history")
        .scrollback;
    let client_hist = sb_synth
        .screen_state_with_scrollback(&client, 0, Some(SCROLLBACK_ALL), false)
        .expect("client history")
        .scrollback;
    assert_eq!(
        client_hist, source_hist,
        "reconstructed history must equal the source's retained rows"
    );
    assert!(
        source_hist.iter().any(|l| l == "line7"),
        "sanity: line7 is the most-recent history row and must survive"
    );
}

/// A bounded request keeps only the most-recent `n` history rows.
#[test]
fn scrollback_snapshot_honors_bounded_limit() {
    let mut source = fresh(20, 4);
    for i in 1..=10 {
        source.vt_write(format!("line{i}\r\n").as_bytes());
    }
    let synth = SnapshotSynthesizer::new().expect("synth");
    let snap = synth
        .synthesize_with_scrollback(&source, Some(2))
        .expect("synthesize_with_scrollback");

    let mut client = fresh(20, 4);
    client.vt_write(&snap.scrollback);
    client.vt_write(&snap.bytes);

    let mut sb_synth = SnapshotSynthesizer::new().expect("synth2");
    let client_hist = sb_synth
        .screen_state_with_scrollback(&client, 0, Some(SCROLLBACK_ALL), false)
        .expect("client history")
        .scrollback;
    // Exactly the 2 most-recent history rows (line6, line7), no more.
    assert_eq!(client_hist, vec!["line6".to_owned(), "line7".to_owned()]);
    assert_eq!(render_grid(&client), render_grid(&source));
}
