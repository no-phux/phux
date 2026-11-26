
use super::*;

const NOW: u64 = 1_795_543_200;

fn outcome(body: &str) -> Outcome {
    classify(
        body.as_bytes(),
        DUMMY_TOKEN.as_bytes(),
        NOW,
        &HashMap::new(),
    )
}

fn with_mac(head: &str) -> String {
    format!("{head}:mac={}", mac_hex(head, DUMMY_TOKEN))
}

#[test]
fn spec_examples_accept_with_their_published_macs() {
    let nvim = "pid=74877:time=1795543200:argv=bnZpbQ==,LVM=,L2hvbWUvdXNlci8ubG9jYWwvc3RhdGUvbnZpbS9zZXNzaW9ucy84ZjJjMWQudmlt:mac=9dbb99bfb856749a87563839d7ce9062818d78cf25909db93f309e0cd3b1909b";
    let clear = "pid=74877:time=1795543200:clear=1:mac=c30603369f98fef581a4098be889175b0c74f0641e49f8b25f4326cb38f97c93";
    let lazy = "pid=74877:time=1795543200:argv=bGF6eWdpdA==:cwd=L2hvbWUvdXNlci9zcmMvcHJvamVjdA==:mac=35cb909805c380336efd3689a125acc774a9dec8422a1d21492ebec06ca758e1";
    let wrap = "pid=90412:time=1795543200:argv=cmVzdW1lLXdyYXA=,c3No,ZGV2Ym94:mac=e76149d0ba54a83570ffab0583867dacdfa23928ba9d18604f0badea88b62b65";
    match outcome(nvim) {
        Outcome::Accepted(parsed) => {
            assert_eq!(
                parsed.argv,
                [
                    "nvim",
                    "-S",
                    "/home/user/.local/state/nvim/sessions/8f2c1d.vim"
                ]
            );
            assert_eq!(parsed.pid, 74877);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(outcome(clear), Outcome::Cleared { pid: 74877 }));
    match outcome(lazy) {
        Outcome::Accepted(parsed) => {
            assert_eq!(parsed.argv, ["lazygit"]);
            assert_eq!(parsed.cwd.as_deref(), Some("/home/user/src/project"));
        }
        other => panic!("{other:?}"),
    }
    match outcome(wrap) {
        Outcome::Accepted(parsed) => {
            assert_eq!(parsed.argv, ["resume-wrap", "ssh", "devbox"]);
            assert_eq!(parsed.pid, 90412);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn invalid_rows_are_ignored_or_rejected_as_the_spec_says() {
    let rows = [
        (
            format!("pid=74877:time={NOW}:argv=bnZpbQ==:mac={}", "0".repeat(64)),
            "ignored",
        ),
        (format!("pid=74877:time={NOW}:argv=bnZpbQ=="), "ignored"),
        (
            with_mac(&format!(
                "mac={}:pid=74877:time={NOW}:argv=bnZpbQ==",
                "ab".repeat(32)
            )),
            "ignored",
        ),
        (
            with_mac("pid=74877:time=1000000000:argv=bnZpbQ=="),
            "ignored",
        ),
        (with_mac("pid=74877:argv=bnZpbQ=="), "ignored"),
        (
            with_mac(&format!("pid=74877:time={NOW}:argv=Li9ydW4uc2g=")),
            "rejected",
        ),
        (
            with_mac(&format!("pid=74877:time={NOW}:argv=bnZpbQ==:clear=1")),
            "rejected",
        ),
        (
            with_mac(&format!(
                "pid=74877:time={NOW}:argv=bnZpbQ==:cwd=cHJvamVjdA=="
            )),
            "rejected",
        ),
        (
            with_mac(&format!("pid=74877:time={NOW}:argv=bnZpbQ==:need=later")),
            "rejected",
        ),
    ];
    // The third row puts a mac pair first. `with_mac` appends the real mac,
    // so the body does not end in the only mac and is ignored. Build that
    // row as the spec writes it: the mac key is present and not last.
    let mac_not_last = format!(
        "mac={}:pid=74877:time={NOW}:argv=bnZpbQ==",
        mac_hex(&format!("pid=74877:time={NOW}:argv=bnZpbQ=="), DUMMY_TOKEN)
    );
    let mut rows = rows;
    rows[2].0 = mac_not_last;
    for (body, label) in rows {
        let got = outcome(&body);
        assert_eq!(got.label(), label, "{body} -> {got:?}");
    }
}

#[test]
fn a_report_split_at_every_byte_accepts_with_bel_or_st() {
    let body = with_mac(&format!("pid=74877:time={NOW}:argv=bnZpbQ=="));
    for terminator in [b"\x07".as_slice(), b"\x1b\\".as_slice()] {
        let mut report = b"\x1b]7577;".to_vec();
        report.extend(body.as_bytes());
        report.extend(terminator);
        let whole = scan_bodies(&[&report]);
        assert!(
            matches!(whole, Some(Outcome::Accepted(_))),
            "{terminator:?}"
        );
        for at in 0..=report.len() {
            let split = scan_bodies(&[&report[..at], &report[at..]]);
            assert_eq!(split, whole, "split at {at}");
        }
    }
}

fn scan_bodies(chunks: &[&[u8]]) -> Option<Outcome> {
    let mut scan = OscScan::new(INTRO, MAX_BODY);
    let mut found = None;
    for chunk in chunks {
        scan.scan(chunk, |body| {
            found = Some(classify(body, DUMMY_TOKEN.as_bytes(), NOW, &HashMap::new()));
        });
    }
    found
}

#[test]
fn sixty_one_seconds_off_and_an_older_time_for_the_same_pid_are_ignored() {
    let head = format!("pid=74877:time={NOW}:argv=bnZpbQ==");
    let body = with_mac(&head);
    assert!(matches!(outcome(&body), Outcome::Accepted(_)));
    for now in [NOW - 61, NOW + 61] {
        assert_eq!(
            classify(
                body.as_bytes(),
                DUMMY_TOKEN.as_bytes(),
                now,
                &HashMap::new()
            )
            .label(),
            "ignored",
            "{now}"
        );
    }
    for now in [NOW - 60, NOW + 60] {
        assert!(
            matches!(
                classify(
                    body.as_bytes(),
                    DUMMY_TOKEN.as_bytes(),
                    now,
                    &HashMap::new()
                ),
                Outcome::Accepted(_)
            ),
            "{now}"
        );
    }
    let mut last = HashMap::new();
    last.insert(74877, NOW);
    let older = with_mac(&format!("pid=74877:time={}:argv=bnZpbQ==", NOW - 1));
    assert_eq!(
        classify(older.as_bytes(), DUMMY_TOKEN.as_bytes(), NOW, &last).label(),
        "ignored"
    );
    let again = classify(body.as_bytes(), DUMMY_TOKEN.as_bytes(), NOW, &last);
    assert!(
        matches!(again, Outcome::Accepted(_)),
        "the same time is not earlier"
    );
}

#[test]
fn a_wrong_mac_does_not_remove_a_record_and_a_replay_inside_the_window_is_one_record() {
    let mut scan = ProgramResumeScan::default();
    scan.set_token(DUMMY_TOKEN.to_owned());
    scan.set_trust_override(Some(true));
    let mut rx = scan.subscribe();
    let good = framed(&with_mac(&format!("pid=74877:time={NOW}:argv=bnZpbQ==")));
    scan.observe(&good, NOW, true);
    let Update::Put(first) = rx.try_recv().expect("record") else {
        panic!("put");
    };
    assert_eq!(first.argv, ["nvim"]);
    assert!(first.trusted);
    let forged = framed(&format!(
        "pid=74877:time={NOW}:argv=bnZpbQ==:mac={}",
        "0".repeat(64)
    ));
    scan.observe(&forged, NOW, true);
    assert!(rx.try_recv().is_err(), "a wrong mac publishes nothing");
    scan.observe(&good, NOW, true);
    let Update::Put(second) = rx.try_recv().expect("replay") else {
        panic!("put");
    };
    assert_eq!(second.argv, first.argv);
    assert_eq!(second.time, first.time);
    scan.observe(&good, NOW + 61, true);
    assert!(rx.try_recv().is_err(), "outside the window");
}

#[test]
fn resets_and_the_alt_screen_do_not_clear_and_clear_or_a_bad_command_does() {
    let mut scan = ProgramResumeScan::default();
    scan.set_token(DUMMY_TOKEN.to_owned());
    let mut rx = scan.subscribe();
    scan.observe(
        &framed(&with_mac(&format!("pid=7:time={NOW}:argv=bnZpbQ=="))),
        NOW,
        true,
    );
    assert!(matches!(rx.try_recv(), Ok(Update::Put(_))));
    for noise in [
        &b"\x1b]133;A\x07"[..],
        b"\x1bc",
        b"\x1b[!p",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
    ] {
        scan.observe(noise, NOW, true);
        assert!(rx.try_recv().is_err(), "{noise:?}");
    }
    scan.observe(
        &framed(&with_mac(&format!("pid=7:time={NOW}:argv=Li9ydW4uc2g="))),
        NOW,
        true,
    );
    assert!(
        matches!(rx.try_recv(), Ok(Update::Clear { pid: 7 })),
        "a rejected command removes the owner"
    );
    scan.observe(
        &framed(&with_mac(&format!("pid=7:time={NOW}:argv=bnZpbQ=="))),
        NOW,
        true,
    );
    assert!(matches!(rx.try_recv(), Ok(Update::Put(_))));
    scan.observe(
        &framed(&with_mac(&format!("pid=7:time={}:clear=1", NOW + 1))),
        NOW,
        true,
    );
    assert!(matches!(rx.try_recv(), Ok(Update::Clear { pid: 7 })));
}

#[test]
fn a_non_shell_parent_is_untrusted_and_no_token_stores_nothing() {
    assert!(direct_child(10, 10, 1000, 1000));
    assert!(!direct_child(10, 11, 1000, 1000));
    assert!(!direct_child(10, 10, 7, 1000));
    assert!(!direct_child(10, 10, 0, 0));
    let mut scan = ProgramResumeScan::default();
    scan.set_token(DUMMY_TOKEN.to_owned());
    scan.set_trust_override(Some(false));
    let mut rx = scan.subscribe();
    scan.observe(
        &framed(&with_mac(&format!("pid=7:time={NOW}:argv=bnZpbQ=="))),
        NOW,
        true,
    );
    let Update::Put(record) = rx.try_recv().expect("stored") else {
        panic!("put");
    };
    assert!(!record.trusted);
    let mut off = ProgramResumeScan::default();
    let mut quiet = off.subscribe();
    off.observe(
        &framed(&with_mac(&format!("pid=7:time={NOW}:argv=bnZpbQ=="))),
        NOW,
        true,
    );
    assert!(quiet.try_recv().is_err());
}

fn framed(body: &str) -> Vec<u8> {
    let mut bytes = b"\x1b]7577;".to_vec();
    bytes.extend(body.as_bytes());
    bytes.push(0x07);
    bytes
}
