#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use super::*;

fn header(idle: Option<f64>) -> CastHeader {
    CastHeader {
        cols: 80,
        rows: 24,
        timestamp: None,
        idle_time_limit: idle,
        command: None,
        title: None,
        env: std::collections::BTreeMap::new(),
        theme: None,
    }
}

fn args(file: &Path, passes: Option<u32>, idle: Option<f64>) -> Request<'_> {
    Request {
        file,
        target: None,
        speed: Speed::NORMAL,
        idle_limit: idle,
        passes,
        split: SpawnSplit::Horizontal,
        ratio: 0.5,
        no_fit: false,
        close: false,
        json: false,
        socket: None,
        pty_writer: false,
    }
}

/// `phux play --json` pins `schema_version` 1 plus the documented result
/// fields.
#[test]
fn play_json_pins_the_contract_shape() {
    let file = Path::new("/home/me/demo.cast");
    let loaded = Loaded {
        header: header(Some(2.0)),
        events: vec![CastEvent {
            time_ms: 0,
            code: EventCode::Output,
            data: "hi".to_owned(),
        }],
        idle_limit: Some(2.0),
    };
    let spec = args(file, Some(1), Some(2.0));
    let pane = ResourceId::local(7);
    let doc = play_json(&pane, file, &loaded, &spec, 17_198);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["terminal_id"], 7);
    assert_eq!(doc["path"], "/home/me/demo.cast");
    assert_eq!(doc["cols"], 80);
    assert_eq!(doc["rows"], 24);
    assert_eq!(doc["events"], 1);
    assert_eq!(doc["speed"], 1.0);
    assert_eq!(doc["idle_limit"], 2.0);
    assert_eq!(doc["duration_ms"], 17_198);
    assert_eq!(doc["passes"], 1);
    assert_eq!(doc.as_object().map(serde_json::Map::len), Some(10));
}

#[test]
fn parse_speed_accepts_the_documented_range_and_names_it_on_failure() {
    assert_eq!(parse_speed("1").expect("1 parses"), Speed::NORMAL);
    let parsed = parse_speed("2.5").expect("2.5 parses").get();
    assert!((parsed - 2.5).abs() < f64::EPSILON, "got {parsed}");
    let err = parse_speed("0").expect_err("zero must be rejected");
    assert!(
        err.contains("0.01"),
        "diagnostic must name the bound: {err}"
    );
    assert!(parse_speed("fast").is_err());
    assert!(parse_speed("-1").is_err());
}

#[test]
fn the_flag_beats_the_header_and_zero_means_no_clamp() {
    // The recording says 2s; the caller says 0.5s. The caller wins.
    assert_eq!(
        effective_idle_limit(&header(Some(2.0)), Some(0.5)),
        Some(0.5)
    );
    // No flag: the recording's own limit is honored, so playback agrees
    // with the recorder that wrote the file.
    assert_eq!(effective_idle_limit(&header(Some(2.0)), None), Some(2.0));
    // Neither: the raw timeline.
    assert_eq!(effective_idle_limit(&header(None), None), None);
    // An explicit 0 disables a header limit rather than clamping to zero
    // — the same spelling `phux rec --idle-limit 0` uses.
    assert_eq!(effective_idle_limit(&header(Some(2.0)), Some(0.0)), None);
    assert_eq!(effective_idle_limit(&header(Some(f64::NAN)), None), None);
}

#[test]
fn writer_argv_round_trips_through_the_cli_parser() {
    // The writer argv the launcher builds must parse in the same binary.

    let file = Path::new("/tmp/demo.cast");
    let mut spec = args(file, Some(3), Some(1.5));
    spec.no_fit = true;
    spec.close = true;
    let argv = writer_argv(
        Path::new("/usr/local/bin/phux"),
        file,
        Path::new("/tmp/phux.sock"),
        &spec,
    );

    let cli = crate::parse_cli(&argv).expect("the writer argv must parse");
    // `--socket` is the root global now, so the writer's copy lands on
    // the top-level field rather than inside the Play variant.
    assert_eq!(cli.socket.as_deref(), Some(Path::new("/tmp/phux.sock")));
    let Some(crate::commands::Command::Play(crate::commands::PlayArgs {
        file: parsed_file,
        speed,
        idle_limit,
        loops,
        no_fit,
        close,
        pty_writer,
        ..
    })) = cli.command
    else {
        panic!("expected the play subcommand");
    };
    assert!(pty_writer, "the pane's process must be in writer mode");
    assert_eq!(parsed_file, file);
    assert_eq!(speed.0, Speed::NORMAL);
    assert_eq!(idle_limit, Some(1.5));
    assert_eq!(loops, Some(3));
    assert!(no_fit);
    assert!(close);
}

#[test]
fn writer_argv_spells_forever_as_the_bare_loop_flag() {
    let file = Path::new("/tmp/demo.cast");
    // `None` passes = play until killed. It must survive the round trip
    // as `Some(0)`, the CLI's spelling of forever — an argv that dropped
    // it would silently turn an infinite loop into a single pass.
    let argv = writer_argv(
        Path::new("/usr/local/bin/phux"),
        file,
        Path::new("/tmp/phux.sock"),
        &args(file, None, None),
    );
    let cli = crate::parse_cli(&argv).expect("the writer argv must parse");
    let Some(crate::Command::Play(crate::commands::PlayArgs { loops, .. })) = cli.command else {
        panic!("expected the play subcommand");
    };
    assert_eq!(loops, Some(0));

    // A single pass is the default on both sides, so it says nothing.
    let single = writer_argv(
        Path::new("/usr/local/bin/phux"),
        file,
        Path::new("/tmp/phux.sock"),
        &args(file, Some(1), None),
    );
    assert!(
        !single.iter().any(|arg| arg == "--loop"),
        "a single pass must not emit --loop: {single:?}"
    );
}

#[test]
fn load_rejects_a_file_that_is_not_a_cast() {
    let dir = std::env::temp_dir().join(format!("phux-play-load-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("not-a-cast.txt");
    std::fs::write(&path, b"this is not asciicast\n").expect("write");
    assert!(
        load(&path, None).is_err(),
        "a non-cast must fail in the launcher, not in the pane"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn load_clamps_with_the_headers_own_limit() {
    let dir = std::env::temp_dir().join(format!("phux-play-clamp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("gap.cast");
    std::fs::write(
        &path,
        b"{\"version\":2,\"width\":80,\"height\":24,\"idle_time_limit\":2.0}\n\
          [0.0, \"o\", \"a\"]\n\
          [30.0, \"o\", \"b\"]\n",
    )
    .expect("write");

    let loaded = load(&path, None).expect("a well-formed cast loads");
    assert_eq!(loaded.idle_limit, Some(2.0));
    assert_eq!(
        loaded.events[1].time_ms, 2_000,
        "a 30s pause under a 2s header limit must collapse to 2s"
    );

    // And the flag overrides it, all the way through to the timeline.
    let loaded = load(&path, Some(0.25)).expect("cast loads");
    assert_eq!(loaded.events[1].time_ms, 250);
    std::fs::remove_dir_all(&dir).ok();
}
