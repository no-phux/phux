//! Output-extremes adversarial stress (crash-hunt wave).
//!
//! Drives the pathological PTY-output shapes the user's lag/crash report
//! implicates, asserting the server's grid synthesis, per-consumer diff,
//! and the client-equivalent oracle all absorb them without a panic and
//! converge to a coherent screen:
//!
//!   * a multi-megabyte burst with NO newlines (one giant logical line —
//!     stresses wrap/reflow and the no-newline grid path);
//!   * a control-character flood (raw bytes 0x00..0x1f sprayed at the VT
//!     parser);
//!   * rapid alt-screen enter/leave toggles (vim/less churn under the
//!     phux-99n fix);
//!   * a wide-glyph / combining / ZWJ flood (the grapheme-cluster paths in
//!     grid synthesis + the oracle's wide-cell skip).
//!
//! Heavy `just e2e` lane only.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

use portable_pty::CommandBuilder;

use phux_server_testkit::builder::E2eBuilder;
use phux_server_testkit::run_local;
use phux_server_testkit::tracing_capture::TracingCapture;

/// A multi-MB burst with no newlines must not panic or wedge the pane.
/// One giant logical line forces continuous wrap/reflow across the grid.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn multi_mb_no_newline_burst_does_not_panic() {
    run_local(async {
        let cap = TracingCapture::install("multi_mb_no_newline");

        // ~2 MB of 'X' with no newline, then a marker on its own line so we
        // can confirm the pane is still alive and parsing afterward.
        //
        // phux-fheq: hold the pane open well past any plausible drain time
        // (was `sleep 30`). On the 2-core CI runner the 2 MB drain through the
        // single-thread runtime can outlast a 30s seed; the shell then exits,
        // the last session is reaped, and the server self-exits — closing the
        // socket while the client is still draining (`UnexpectedEof`). A
        // long-lived seed decouples the assertion from wall clock; the harness
        // tears the shell down via the shutdown cascade when the test ends.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "yes X | head -c 2000000 | tr -d '\\n'; printf '\\nNONLDONE\\n'; sleep 100000",
        ]);

        E2eBuilder::new()
            .session("default")
            .seed_cmd(cmd)
            .viewport(80, 24)
            .run(|mut clients| async move {
                let client = &mut clients[0];
                // phux-fheq: the 2 MB no-newline reflow drains legitimately
                // slowly through the single-thread runtime on a 2-core CI
                // runner — well past the default 15s WIRE_RECV_TIMEOUT (the
                // marker arrived ~33s in). Give this one a generous budget so
                // the slow-but-correct drain isn't mistaken for a hang; a
                // genuinely wedged pane still fails at the ceiling.
                let res = client
                    .wait_until_with_timeout(std::time::Duration::from_secs(180), |s| {
                        s.contains("NONLDONE")
                    })
                    .await;
                cap.attach_screen(client.screenshot().await.snapshot_text());
                assert!(
                    res.is_ok(),
                    "pane never reached the post-burst marker after a 2MB \
                     no-newline burst; screen=\n{}",
                    res.unwrap_err(),
                );
            })
            .await;
    });
}

/// A control-character flood (raw 0x01..0x1f sprayed at the parser) must
/// not panic the VT path. The marker afterward proves recovery.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn control_char_flood_does_not_panic() {
    run_local(async {
        let cap = TracingCapture::install("control_char_flood");

        // printf a run of assorted control bytes (BEL, BS, VT, FF, ESC
        // fragments, SO/SI, etc.), repeated, then a clean marker line.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "for i in $(seq 1 200); do \
               printf '\\001\\002\\007\\010\\013\\014\\016\\017\\033\\033[\\033]'; \
             done; printf '\\033[0m\\nCTRLDONE\\n'; sleep 30",
        ]);

        E2eBuilder::new()
            .session("default")
            .seed_cmd(cmd)
            .viewport(80, 24)
            .run(|mut clients| async move {
                let client = &mut clients[0];
                let res = client.wait_until(|s| s.contains("CTRLDONE")).await;
                cap.attach_screen(client.screenshot().await.snapshot_text());
                assert!(
                    res.is_ok(),
                    "pane never recovered after a control-char flood; \
                     screen=\n{}",
                    res.unwrap_err(),
                );
            })
            .await;
    });
}

/// Rapid alt-screen enter/leave toggles (the vim/less churn) must not
/// panic or strand the grid. Under the phux-99n fix the alt-screen
/// transitions resync cleanly.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn rapid_alt_screen_toggles_do_not_panic() {
    run_local(async {
        let cap = TracingCapture::install("alt_screen_toggles");

        // Toggle DEC 1049 (alt screen + save/restore cursor) many times,
        // writing a little content in each mode, then settle on the
        // primary screen with a marker.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "for i in $(seq 1 100); do \
               printf '\\033[?1049h'; printf 'ALT-%d' \"$i\"; \
               printf '\\033[?1049l'; printf 'PRIMARY-%d\\r\\n' \"$i\"; \
             done; printf 'ALTDONE\\r\\n'; sleep 30",
        ]);

        E2eBuilder::new()
            .session("default")
            .seed_cmd(cmd)
            .viewport(80, 24)
            .run(|mut clients| async move {
                let client = &mut clients[0];
                let res = client.wait_until(|s| s.contains("ALTDONE")).await;
                cap.attach_screen(client.screenshot().await.snapshot_text());
                assert!(
                    res.is_ok(),
                    "pane never reached the marker after rapid alt-screen \
                     toggles; screen=\n{}",
                    res.unwrap_err(),
                );
            })
            .await;
    });
}

/// A flood of wide glyphs, combining marks, and ZWJ emoji sequences must
/// not panic the grapheme-cluster paths (grid synthesis or the oracle's
/// wide-cell-tail skip). The marker afterward proves the pane survived.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn wide_combining_zwj_flood_does_not_panic() {
    run_local(async {
        let cap = TracingCapture::install("wide_zwj_flood");

        // Mix: CJK wide chars, a base + combining diacritics stack, and a
        // ZWJ family emoji, repeated densely, with periodic newlines so the
        // grid wraps and reflows through the grapheme paths.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "for i in $(seq 1 150); do \
               printf '\\344\\275\\240\\345\\245\\275'; \
               printf 'e\\314\\201e\\314\\200e\\314\\202'; \
               printf '\\360\\237\\221\\250\\342\\200\\215\\360\\237\\221\\251\\342\\200\\215\\360\\237\\221\\247'; \
               if [ $((i % 5)) -eq 0 ]; then printf '\\r\\n'; fi; \
             done; printf '\\r\\nZWJDONE\\r\\n'; sleep 30",
        ]);

        E2eBuilder::new()
            .session("default")
            .seed_cmd(cmd)
            .viewport(80, 24)
            .run(|mut clients| async move {
                let client = &mut clients[0];
                let res = client.wait_until(|s| s.contains("ZWJDONE")).await;
                cap.attach_screen(client.screenshot().await.snapshot_text());
                assert!(
                    res.is_ok(),
                    "pane never reached the marker after a wide/combining/ZWJ \
                     flood (grapheme path panic?); screen=\n{}",
                    res.unwrap_err(),
                );
            })
            .await;
    });
}
