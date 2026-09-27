//! Pathological PTY output must not panic grid synthesis, the per-consumer
//! diff, or the client oracle; a marker printed afterwards proves the pane
//! is still alive and parsing.

use std::time::Duration;

use portable_pty::CommandBuilder;

use phux_server_testkit::builder::E2eBuilder;
use phux_server_testkit::run_local;
use phux_server_testkit::tracing_capture::TracingCapture;

/// Run `script` (which must print `marker` last and then idle) and wait up
/// to `budget` for the marker to render.
fn survives(label: &str, script: &str, marker: &'static str, budget: Duration) {
    run_local(async {
        let cap = TracingCapture::install(label);
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", script]);
        E2eBuilder::new()
            .seed_cmd(cmd)
            .run(|mut clients| async move {
                let client = &mut clients[0];
                let res = client
                    .wait_until_with_timeout(budget, |s| s.contains(marker))
                    .await;
                cap.attach_screen(client.screenshot().await.snapshot_text());
                res.unwrap_or_else(|screen| panic!("never reached {marker}; screen=\n{screen}"));
            })
            .await;
    });
}

/// ~2 MB with no newline: one giant logical line reflowed across the grid.
/// The drain is legitimately slow on a 2-core runner, hence the budget; the
/// seed idles long after so the server cannot self-exit mid-drain.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just stress`."]
#[test]
fn multi_mb_no_newline_burst_does_not_panic() {
    survives(
        "multi_mb_no_newline",
        "yes X | head -c 2000000 | tr -d '\\n'; printf '\\nNONLDONE\\n'; sleep 100000",
        "NONLDONE",
        Duration::from_secs(180),
    );
}

/// Control bytes, rapid DEC 1049 alt-screen toggles, and a wide/combining/ZWJ
/// flood, each followed by its own marker.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just stress`."]
#[test]
fn control_alt_screen_and_grapheme_floods_do_not_panic() {
    let budget = Duration::from_secs(15);
    survives(
        "control_char_flood",
        "for i in $(seq 1 200); do printf '\\001\\002\\007\\010\\013\\014\\016\\017\\033\\033[\\033]'; \
         done; printf '\\033[0m\\nCTRLDONE\\n'; sleep 30",
        "CTRLDONE",
        budget,
    );
    survives(
        "alt_screen_toggles",
        "for i in $(seq 1 100); do printf '\\033[?1049h'; printf 'ALT-%d' \"$i\"; \
         printf '\\033[?1049l'; printf 'PRIMARY-%d\\r\\n' \"$i\"; done; printf 'ALTDONE\\r\\n'; sleep 30",
        "ALTDONE",
        budget,
    );
    survives(
        "wide_zwj_flood",
        "for i in $(seq 1 150); do printf '\\344\\275\\240\\345\\245\\275'; \
         printf 'e\\314\\201e\\314\\200e\\314\\202'; \
         printf '\\360\\237\\221\\250\\342\\200\\215\\360\\237\\221\\251\\342\\200\\215\\360\\237\\221\\247'; \
         if [ $((i % 5)) -eq 0 ]; then printf '\\r\\n'; fi; done; printf '\\r\\nZWJDONE\\r\\n'; sleep 30",
        "ZWJDONE",
        budget,
    );
}
