use super::*;
use crate::commands::doctor::Status;

/// Co-occurring server-health conditions are all reported, and the pass
/// fallback count is not read.
#[test]
fn every_applicable_server_health_condition_is_reported() {
    let unit = std::path::Path::new("/home/u/.config/systemd/user/phux.service");
    let checks = server_health_checks(
        Some((9, 60)),
        Some(unit),
        None,
        Some(("0.13.0", "0.14.0")),
        None,
        || panic!("recent_starts must not be read once another condition already applies"),
    );

    assert_eq!(checks.len(), 3, "{checks:?}");
    assert_eq!(checks[0].status, Status::Fail);
    assert!(
        checks[0].detail.contains("crash-looping"),
        "{}",
        checks[0].detail
    );
    assert_eq!(checks[1].status, Status::Warn);
    assert!(
        checks[1].detail.contains(&unit.display().to_string()),
        "{}",
        checks[1].detail
    );
    assert_eq!(checks[2].status, Status::Warn);
    assert!(checks[2].detail.contains("0.13.0"), "{}", checks[2].detail);
    assert!(checks[2].detail.contains("0.14.0"), "{}", checks[2].detail);
}

/// Version remedies follow release ordering, never lexical ordering, and never
/// tell an older client to replace a newer server. Unordered checkout builds
/// keep the same hand-over policy as auto-attach.
#[test]
fn version_skew_hint_updates_the_older_side() {
    for (theirs, ours, remedy, forbidden) in [
        ("0.13.0", "0.14.0", "phux upgrade", "phux update"),
        ("0.14.0", "0.13.0", "phux update", "phux upgrade"),
        ("0.9.0", "0.10.0", "phux upgrade", "phux update"),
        ("0.10.0", "0.9.0", "phux update", "phux upgrade"),
        ("1.0.0", "0.99.0", "phux update", "phux upgrade"),
        ("0.43.0-dev", "0.43.0", "phux upgrade", "phux update"),
        ("0.43.0", "next", "phux upgrade", "phux update"),
    ] {
        let checks = server_health_checks(None, None, None, Some((theirs, ours)), None, || {
            panic!("version skew must not report a clean host")
        });
        assert_eq!(checks.len(), 1, "{checks:?}");
        assert_eq!(checks[0].status, Status::Warn);
        assert!(checks[0].detail.contains(theirs), "{checks:?}");
        assert!(checks[0].detail.contains(ours), "{checks:?}");
        let hint = checks[0]
            .hint
            .as_deref()
            .expect("version skew needs a remedy");
        assert!(hint.contains(remedy), "{theirs} vs {ours}: {hint}");
        assert!(!hint.contains(forbidden), "{theirs} vs {ours}: {hint}");
    }
}

/// A live server that refused this protocol gets the stop-and-start recovery,
/// not the same-major `phux upgrade` hand-over.
#[test]
fn an_incompatible_version_skew_names_the_stop_recovery() {
    let socket = std::path::Path::new("/tmp/old.sock");
    let checks = server_health_checks(
        None,
        None,
        None,
        Some(("0.9.0", "1.4.0")),
        Some((socket, Some(4242))),
        || panic!("version skew must not report a clean host"),
    );
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0].status, Status::Warn);
    let hint = checks[0]
        .hint
        .as_deref()
        .expect("incompatible skew needs a remedy");
    assert!(hint.contains("kill 4242"), "{hint}");
    assert!(hint.contains("pkill -f 'phux server'"), "{hint}");
    assert!(hint.contains("`phux server`"), "{hint}");
    assert!(hint.contains("closes the panes"), "{hint}");
    for forbidden in ["phux upgrade", "stale", "survive", "remove", "in place"] {
        assert!(!hint.contains(forbidden), "{forbidden} in {hint}");
    }

    let unknown = server_health_checks(
        None,
        None,
        None,
        Some(("0.9.0", "1.4.0")),
        Some((socket, None)),
        || panic!("version skew must not report a clean host"),
    );
    let hint = unknown[0].hint.as_deref().expect("hint");
    assert!(hint.contains("pkill -f 'phux server'"), "{hint}");
    assert!(!hint.contains("`kill "), "{hint}");
    assert!(!hint.contains("remove"), "{hint}");
}

/// A clean host gets exactly one pass line.
#[test]
fn server_health_passes_when_nothing_applies() {
    let checks = server_health_checks(None, None, None, None, None, || 2);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, Status::Pass);
    assert!(checks[0].detail.contains("2 server start(s)"));
}

/// Armed supervision is a warning carrying the shared explanation verbatim.
#[test]
fn armed_supervision_is_surfaced_as_informational() {
    let unit = std::path::Path::new("/home/u/Library/LaunchAgents/com.phux.server.plist");
    let checks = server_health_checks(None, None, Some(unit), None, None, || {
        panic!("recent_starts must not be read once another condition already applies")
    });

    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0].status, Status::Warn);
    assert!(checks[0].detail.contains("armed"), "{}", checks[0].detail);
    assert!(
        checks[0].detail.contains(&unit.display().to_string()),
        "{}",
        checks[0].detail
    );
    let hint = checks[0]
        .hint
        .as_ref()
        .expect("a warn without a hint is half a diagnosis");
    assert!(
        hint.contains("--adopt") && hint.contains("working as designed"),
        "the hint must name the state's origin and that it is intended: {hint}"
    );
    assert!(
        hint.contains("not caught by anything"),
        "the one real risk of the armed window must be stated: {hint}"
    );
    assert!(
        hint.contains("service uninstall"),
        "the way out must be named: {hint}"
    );
    assert!(
        hint.contains(crate::commands::service::ARMED_SUPERVISION_EXPLANATION),
        "the hint must carry the shared explanation verbatim, not a second copy: {hint}"
    );
}

/// The legacy-unit hint names the non-destructive `reconcile`, not
/// `install`, and says the macOS fix lands at next login.
#[test]
fn legacy_unit_hint_stays_honest_about_its_own_remedy() {
    let unit = std::path::Path::new("/home/u/Library/LaunchAgents/com.phux.server.plist");
    let checks = server_health_checks(None, Some(unit), None, None, None, || 0);
    let hint = checks[0]
        .hint
        .as_ref()
        .expect("a warn without a hint is half a diagnosis");
    assert!(hint.contains("service reconcile"), "{hint}");
    assert!(
        !hint.contains("service install"),
        "the hint must not send a user at the pane-killing remedy now that \
         a non-destructive one exists (phux-nvi2, phux-l1yx): {hint}"
    );
    assert!(
        hint.contains("no pane is lost"),
        "the remedy's zero cost is the reason it replaced the old one; say it: {hint}"
    );
    assert!(
        hint.contains("next login"),
        "on macOS the policy is not in force until then, and a hint that \
         implies otherwise is the same dishonesty nvi2 was filed for: {hint}"
    );
}

/// A `ServicePlan` for feeding the real `service` renderers.
fn service_plan_fixture() -> crate::commands::service::ServicePlan {
    crate::commands::service::ServicePlan {
        binary: PathBuf::from("/usr/local/bin/phux"),
        quic: None,
        listen: None,
        tokens: PathBuf::from("/home/u/.local/state/phux/remote-tokens"),
        cert: PathBuf::from("/home/u/.local/state/phux/remote-cert.pem"),
        key: PathBuf::from("/home/u/.local/state/phux/remote-key.pem"),
        socket: None,
        hub: false,
        socket_path: PathBuf::from("/tmp/phux.sock"),
        profile: None,
        log: PathBuf::from("/home/u/.local/state/phux/server.log"),
        restore: None,
        wrapper: PathBuf::from("/home/u/.local/state/phux/service-wrapper.sh"),
    }
}

/// A unit doctor cannot read is not guessed legacy.
#[test]
fn an_unreadable_unit_is_not_flagged_legacy() {
    let path = std::path::Path::new("/nonexistent/phux-doctor-test/unit-file");
    assert!(!supervisor_unit_is_legacy(path));
}

/// The legacy verdict for fresh units, the pre-throttle shape, a zero
/// throttle, and `Restart=always` beside a stray `SuccessfulExit`, and its
/// agreement with `reconcile_unit` on each.
#[test]
#[allow(clippy::unwrap_used, reason = "test code")]
fn supervisor_unit_is_legacy_and_reconcile_unit_agree_on_known_cases() {
    use crate::commands::service::{Manager, Reconcile, reconcile_unit};

    let dir = tempfile::tempdir().unwrap();
    let cases: [(&str, Manager, String, bool); 5] = [
        (
            "fresh-launchd",
            Manager::Launchd,
            crate::commands::service::render_launchd_plist(&service_plan_fixture()),
            false,
        ),
        (
            "fresh-systemd",
            Manager::Systemd,
            crate::commands::service::render_systemd_unit(&service_plan_fixture()),
            false,
        ),
        (
            "pre-zomb4-launchd",
            Manager::Launchd,
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n  \
             <key>Label</key>\n  <string>com.phux.server</string>\n  \
             <key>KeepAlive</key>\n  <true/>\n</dict>\n</plist>\n"
                .to_owned(),
            true,
        ),
        (
            "zero-throttle-launchd",
            Manager::Launchd,
            "<plist version=\"1.0\">\n<dict>\n  <key>SuccessfulExit</key>\n    <false/>\n  \
             <key>ThrottleInterval</key>\n  <integer>0</integer>\n</dict>\n</plist>\n"
                .to_owned(),
            true,
        ),
        (
            "restart-always-systemd",
            Manager::Systemd,
            "[Service]\n# SuccessfulExit is launchd's spelling, not used here\n\
             Restart=always\nRestartSec=30s\n"
                .to_owned(),
            true,
        ),
    ];

    for (name, manager, body, expected) in cases {
        let path = dir.path().join(name);
        std::fs::write(&path, &body).unwrap();

        let legacy = supervisor_unit_is_legacy(&path);
        assert_eq!(legacy, expected, "{name}: legacy verdict");
        let would_change = !matches!(reconcile_unit(manager, &body), Reconcile::Current);
        assert_eq!(
            legacy,
            would_change,
            "{name}: supervisor_unit_is_legacy={legacy} but reconcile_unit \
             {}Current",
            if would_change { "!= " } else { "== " }
        );
    }
}

/// The accepted divergence: a retuned positive throttle is not legacy, while
/// `reconcile_unit` still converges it.
#[test]
#[allow(clippy::unwrap_used, reason = "test code")]
fn a_retuned_but_positive_throttle_is_not_legacy_though_reconcile_would_still_rewrite_it() {
    use crate::commands::service::{Manager, Reconcile, reconcile_unit};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("com.phux.server.plist");
    let body = crate::commands::service::render_launchd_plist(&service_plan_fixture());
    let current = body
        .split_once("<key>ThrottleInterval</key>")
        .and_then(|(_, rest)| plist_integer(rest))
        .expect("today's renderer always emits a positive ThrottleInterval");
    let retuned = body.replacen(
        &format!("<integer>{current}</integer>"),
        &format!("<integer>{}</integer>", current + 1),
        1,
    );
    assert_ne!(
        retuned, body,
        "the substitution must actually change something"
    );
    std::fs::write(&path, &retuned).unwrap();

    assert!(
        !supervisor_unit_is_legacy(&path),
        "a positive throttle is safe regardless of its exact number"
    );
    assert_ne!(
        reconcile_unit(Manager::Launchd, &retuned),
        Reconcile::Current,
        "reconcile still wants to converge on the exact current throttle value"
    );
}
