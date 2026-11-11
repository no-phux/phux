use super::*;
use crate::state::mirror_demand::RUNNER_FRESH;

fn entry(target: &str) -> MirrorConfigEntry {
    MirrorConfigEntry {
        target: target.to_owned(),
        format: None,
        quality: None,
        max_width: None,
        interval_ms: None,
    }
}

#[test]
fn starting_is_idempotent_and_never_duplicates_a_configured_mirror() {
    let state = SharedState::new();
    // The macOS runner must have reported; Linux ignores it.
    assert!(matches!(provide(&state, "[]"), CommandResult::OkWith(_)));
    state.with_mut(|s| s.mirror_demand_mut().config = vec![entry("display")]);
    let first = want(&state, "firefox:Docs", true).expect("wanted");
    let again = want(&state, " firefox:Docs ", false).expect("wanted");
    assert_eq!(first, "mirror-firefox-docs");
    assert_eq!(first, again);
    assert_eq!(
        want(&state, "display", false).ok().as_deref(),
        Some("mirror-display")
    );
    let wanted = state.with(|s| s.mirror_demand().wanted());
    let names: Vec<_> = wanted.iter().map(MirrorConfigEntry::session_name).collect();
    assert_eq!(names, ["mirror-display", "mirror-firefox-docs"]);
    assert_eq!(
        wanted[1].format,
        Some(MirrorFormat::Jpeg),
        "the first start's format stays"
    );
    assert!(want(&state, "\u{7}", false).is_err());
    assert!(want(&state, "--", false).is_err());
}

#[test]
fn stopping_ends_a_started_mirror_but_not_a_configured_one() {
    let state = SharedState::new();
    let _ = provide(&state, "[]");
    state.with_mut(|s| s.mirror_demand_mut().config = vec![entry("display")]);
    want(&state, "kitty", false).expect("wanted");
    assert!(matches!(stop(&state, "kitty"), CommandResult::OkWith(_)));
    assert!(matches!(
        stop(&state, "kitty"),
        CommandResult::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));
    assert!(matches!(
        stop(&state, "display"),
        CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            ..
        }
    ));
}

#[test]
fn the_runner_learns_every_wanted_mirror_and_goes_stale() {
    let state = SharedState::new();
    state.with_mut(|s| s.mirror_demand_mut().config = vec![entry("display")]);
    let CommandResult::OkWith(CommandValue::Json(reply)) = provide(
        &state,
        r#"[{"target":"display","kind":"display","label":"Main display"}]"#,
    ) else {
        panic!("provide answers JSON");
    };
    let reply: serde_json::Value = serde_json::from_str(&reply).expect("json");
    assert_eq!(reply["mirrors"][0]["session"], "mirror-display");
    assert_eq!(reply["mirrors"][0]["args"][0], "agent");
    let later = Instant::now() + RUNNER_FRESH;
    state.with(|s| {
        assert!(s.mirror_demand().fresh_targets(Instant::now()).is_some());
        assert!(s.mirror_demand().fresh_targets(later).is_none());
    });
    assert!(matches!(
        provide(&state, "{}"),
        CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            ..
        }
    ));
}

#[test]
fn the_cli_listing_is_parsed_or_its_error_reported() {
    let ok =
        br#"{"schema_version":1,"targets":[{"target":"display","kind":"display","label":"x"}]}"#;
    assert_eq!(parse_targets(ok).expect("targets")[0]["target"], "display");
    let err = br#"{"schema_version":1,"error":{"code":"workload","message":"no hyprland"}}"#;
    assert_eq!(parse_targets(err).unwrap_err(), "no hyprland");
    assert!(parse_targets(b"usage: phux").is_err());
}
