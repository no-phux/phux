use super::reconcile::tests::{LEGACY_PLIST, LEGACY_UNIT, patched};
use super::unit::tests::plan;
use super::{
    HubEnsure, Manager, Reconcile, arm_unit, config_home_from, dry_run_text, ensure_hub_in_unit,
    ensure_hub_in_wrapper, ensure_paired_policy, home_dir_from, render_launchd_plist, render_unit,
    render_wrapper_script, report_policy_reach_with, resolve_plan, rewrite_unit_binary,
    status_report, unit_socket_override, unit_supervises,
};
use std::path::Path;
use std::path::PathBuf;

/// A captured init-system invocation, for driving [`status_report`]
/// without an init system. `raw` is a wait(2) status: `0` is success,
/// `1 << 8` is exit code 1.
fn probe_output(raw: i32, stdout: &str, stderr: &str) -> std::process::Output {
    use std::os::unix::process::ExitStatusExt;
    std::process::Output {
        status: std::process::ExitStatus::from_raw(raw),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

/// What `launchctl print` writes to stderr for a job that is not loaded.
const LAUNCHCTL_NOT_FOUND_STDERR: &str =
    "Bad request.\nCould not find service \"com.phux.server\" in domain for user gui: 501\n";

#[test]
fn silent_systemd_policy_reconciliation_still_reloads_the_unit() {
    let called = std::cell::Cell::new(false);
    report_policy_reach_with(
        Manager::Systemd,
        std::path::Path::new("unused"),
        false,
        false,
        |program, args| {
            called.set(true);
            assert_eq!(program, "systemctl");
            assert_eq!(args, ["--user", "daemon-reload"]);
            Ok(())
        },
    );
    assert!(
        called.get(),
        "JSON mode must suppress output, not side effects"
    );
}

/// The status report is phux's verdict plus the probe's stdout, never its
/// stderr. An armed unit's not-found answer is the expected state
/// (ADR-0088), not a fault; armed plus running means the hand-over
/// completed (login bootstrap does not clear the marker), which is the
/// caller's cue to sweep the stale record; neither armed nor running is
/// the one combination that exits non-zero.
#[test]
fn status_reports_speak_phux_vocabulary_and_drop_the_tools_stderr() {
    let armed_text = super::ARMED_SUPERVISION_EXPLANATION;
    for (armed, raw, stdout, stderr, running, present, absent) in [
        (
            true,
            1 << 8,
            "",
            LAUNCHCTL_NOT_FOUND_STDERR,
            false,
            &["state armed", armed_text, "expected state, not a fault"][..],
            &[
                "Bad request",
                "Could not find service",
                "installed, but the init system",
            ][..],
        ),
        (
            false,
            3 << 8,
            "Active: inactive (dead)\n",
            LAUNCHCTL_NOT_FOUND_STDERR,
            false,
            &[
                "installed, but the init system is not running it",
                "Active: inactive (dead)",
            ],
            &["Bad request"],
        ),
        (
            true,
            0,
            "com.phux.server = {\n\tstate = running\n}\n",
            "",
            true,
            &["has completed", "state = running"],
            &["state armed"],
        ),
        (
            false,
            0,
            "com.phux.server = {\n}\n",
            "noise on stderr\n",
            true,
            &["com.phux.server"],
            &["noise on stderr"],
        ),
    ] {
        let report =
            status_report(armed, || Ok(probe_output(raw, stdout, stderr))).expect("probe ran");
        assert_eq!(report.running, running, "{}", report.text);
        for needle in present {
            assert!(report.text.contains(needle), "{needle}: {}", report.text);
        }
        for needle in absent {
            assert!(!report.text.contains(needle), "{needle}: {}", report.text);
        }
    }
}

/// A unit with a remote listener writes `paired` where no mode is set,
/// keeps an existing `paired`, and refuses `local`; a unit without one
/// leaves the config alone (ADR-0179).
#[test]
fn a_remote_listener_unit_settles_the_policy_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");

    let mut local_only = plan();
    local_only.quic = None;
    local_only.listen = None;
    ensure_paired_policy(&local_only, &config, false).expect("no listener, nothing to settle");
    assert!(!config.exists(), "the config is left alone");

    std::fs::write(&config, "[sidebar]\nwidth = 30\n").expect("config");
    ensure_paired_policy(&plan(), &config, false).expect("an unset mode becomes paired");
    let written = std::fs::read_to_string(&config).expect("config");
    assert!(
        written.contains("width = 30") && written.contains("mode = \"paired\""),
        "{written}"
    );
    ensure_paired_policy(&plan(), &config, false).expect("paired stays");
    assert_eq!(std::fs::read_to_string(&config).expect("config"), written);

    std::fs::write(&config, "[policy]\nmode = \"local\"\n").expect("config");
    let refused = ensure_paired_policy(&plan(), &config, false).expect_err("local refuses");
    assert!(refused.contains("\"local\""), "{refused}");
    assert_eq!(
        std::fs::read_to_string(&config).expect("config"),
        "[policy]\nmode = \"local\"\n",
        "a refused install changes nothing"
    );
}

/// A relay-only host (`[[connector]]`) or one whose server inherits a
/// listener address opens a remote entry point too, so its unit settles
/// the mode the same way rather than crash-looping (ADR-0179).
#[test]
fn a_connector_or_inherited_listener_settles_the_policy_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    let mut local_only = plan();
    local_only.quic = None;
    local_only.listen = None;

    let connector = "[[connector]]\nrelay = \"wss://relay.example:443\"\n";
    std::fs::write(&config, connector).expect("config");
    ensure_paired_policy(&local_only, &config, false).expect("a connector becomes paired");
    let written = std::fs::read_to_string(&config).expect("config");
    assert!(
        written.contains("relay.example") && written.contains("mode = \"paired\""),
        "{written}"
    );

    std::fs::write(&config, format!("{connector}[policy]\nmode = \"local\"\n")).expect("config");
    let refused = ensure_paired_policy(&local_only, &config, false).expect_err("local refuses");
    assert!(refused.contains("[[connector]]"), "{refused}");

    std::fs::write(&config, "").expect("config");
    ensure_paired_policy(&local_only, &config, true)
        .expect("an inherited listener address becomes paired");
    let written = std::fs::read_to_string(&config).expect("config");
    assert!(written.contains("mode = \"paired\""), "{written}");
}

/// Unit paths are per-user and profile-scoped, so a dev install cannot
/// overwrite the production unit file.
#[test]
fn a_non_default_profile_writes_its_unit_beside_the_default_one() {
    for (manager, default_name, dev_name) in [
        (
            Manager::Launchd,
            "Library/LaunchAgents/com.phux.server.plist",
            "com.phux.server.dev.plist",
        ),
        (
            Manager::Systemd,
            "systemd/user/phux.service",
            "phux-dev.service",
        ),
    ] {
        let default = manager.unit_path(None).expect("HOME is set");
        let dev = manager.unit_path(Some("dev")).expect("HOME is set");
        assert!(default.ends_with(default_name), "{}", default.display());
        assert!(dev.ends_with(dev_name), "{}", dev.display());
        assert_eq!(default.parent(), dev.parent());
    }
}

/// Tests are dev builds: the production unit is refused to every writer,
/// the dev profile's unit beside it is not.
#[test]
fn a_dev_build_is_refused_the_production_unit() {
    for manager in [Manager::Launchd, Manager::Systemd] {
        let default = manager.unit_path(None).expect("HOME is set");
        assert_eq!(
            manager.writable_unit_path(None).is_err(),
            phux_config::production::refuse_dev_on_production_state(&default).is_err(),
            "{}",
            default.display()
        );
        assert!(manager.writable_unit_path(Some("dev")).is_ok());
    }
}

/// The plist label follows the plan's profile.
#[test]
fn the_plist_label_follows_the_plans_profile() {
    let default_plist = render_launchd_plist(&plan());
    assert!(
        default_plist.contains("<string>com.phux.server</string>"),
        "got {default_plist}"
    );

    let mut dev = plan();
    dev.profile = Some("dev".to_owned());
    let dev_plist = render_launchd_plist(&dev);
    assert!(
        dev_plist.contains("<string>com.phux.server.dev</string>"),
        "got {dev_plist}"
    );
    assert!(
        !dev_plist.contains("<string>com.phux.server</string>\n"),
        "the dev label must not also emit the bare production one: {dev_plist}"
    );
}

/// A dry run renders on every platform; with no generator it is the systemd
/// unit (ADR-0055).
#[test]
fn a_dry_run_renders_even_with_no_unit_generator_for_the_platform() {
    let text = dry_run_text(None, &plan());
    assert!(
        !text.is_empty(),
        "an unsupported platform must still get a unit to adapt"
    );
    assert_eq!(text, dry_run_text(Some(Manager::Systemd), &plan()));
    assert!(text.contains("[Service]"), "got {text}");
}

/// With `HOME` (and `XDG_CONFIG_HOME`) unset or empty, unit paths are
/// refused rather than resolved relative to the cwd.
#[test]
fn unit_path_errors_instead_of_writing_into_the_cwd_when_home_is_unset() {
    // An empty (but present) HOME is exactly as absent as an unset one.
    for refused in [
        home_dir_from(None),
        home_dir_from(Some(std::ffi::OsString::new())),
        config_home_from(None, None),
    ] {
        assert!(refused.expect_err("refused").contains("HOME"));
    }
    // XDG_CONFIG_HOME alone is enough even with HOME unset.
    let config = config_home_from(Some("/xdg-config".into()), None);
    assert_eq!(config, Ok(PathBuf::from("/xdg-config")));
}

#[test]
fn rewrite_unit_binary_points_launchd_and_systemd_at_the_new_install() {
    let next = Path::new("/Users/me/.local/bin/phux");
    let launchd = patched(rewrite_unit_binary(Manager::Launchd, LEGACY_PLIST, next));
    assert!(
        launchd.contains("<string>/Users/me/.local/bin/phux</string>"),
        "{launchd}"
    );
    assert!(
        !launchd.contains("<string>/usr/local/bin/phux</string>"),
        "{launchd}"
    );
    assert!(
        launchd.contains("<string>--hub</string>"),
        "binary rewrite must not drop flags:\n{launchd}"
    );

    let systemd = patched(rewrite_unit_binary(Manager::Systemd, LEGACY_UNIT, next));
    assert!(
        systemd.contains("ExecStart=/Users/me/.local/bin/phux server --hub"),
        "{systemd}"
    );
    assert!(
        systemd.contains("Environment=\"PHUX_QUIC_ADDR=0.0.0.0:8788\""),
        "binary rewrite must not drop env:\n{systemd}"
    );
}

#[test]
fn rewrite_unit_binary_is_a_fixed_point_when_the_path_already_matches() {
    let already = Path::new("/usr/local/bin/phux");
    assert_eq!(
        rewrite_unit_binary(Manager::Launchd, LEGACY_PLIST, already),
        Reconcile::Current
    );
    assert_eq!(
        rewrite_unit_binary(Manager::Systemd, LEGACY_UNIT, already),
        Reconcile::Current
    );
}

/// Patching `--hub` into a generated unit equals generating with `--hub`, and
/// patching again is a no-op.
#[test]
fn ensuring_hub_matches_generating_with_hub() {
    for manager in [Manager::Launchd, Manager::Systemd] {
        let without = render_unit(manager, &plan());
        let mut with_hub = plan();
        with_hub.hub = true;
        let expected = render_unit(manager, &with_hub);
        match ensure_hub_in_unit(manager, &without) {
            HubEnsure::Patched(patched) => assert_eq!(
                patched, expected,
                "{manager:?} --hub patch drifted from the hub=true renderer"
            ),
            other => panic!("{manager:?} expected a patch, got {other:?}\n{without}"),
        }
        assert!(
            matches!(ensure_hub_in_unit(manager, &expected), HubEnsure::Current),
            "{manager:?} a hub unit must be a no-op to patch"
        );
    }
}

/// With `--restore` the unit execs `/bin/sh`, so `--hub` goes into the
/// wrapper's server line only.
#[test]
fn ensuring_hub_on_a_restore_unit_names_the_wrapper() {
    let mut plan = plan();
    plan.restore = Some(PathBuf::from("/home/u/.local/state/phux/workspace.json"));
    for manager in [Manager::Launchd, Manager::Systemd] {
        match ensure_hub_in_unit(manager, &render_unit(manager, &plan)) {
            HubEnsure::Wrapper(path) => assert_eq!(path, plan.wrapper),
            other => panic!("{manager:?} expected Wrapper, got {other:?}"),
        }
    }

    let without = render_wrapper_script(&plan);
    plan.hub = true;
    let expected = render_wrapper_script(&plan);
    match ensure_hub_in_wrapper(&without) {
        HubEnsure::Patched(patched) => assert_eq!(patched, expected),
        other => panic!("expected a wrapper patch, got {other:?}\n{without}"),
    }
    assert!(matches!(
        ensure_hub_in_wrapper(&expected),
        HubEnsure::Current
    ));
    // The archive export line must not gain --hub.
    assert!(
        !expected.contains("archive export --hub"),
        "only the server start line takes --hub:\n{expected}"
    );
}

#[test]
fn ensuring_hub_refuses_an_unparseable_unit() {
    assert!(matches!(
        ensure_hub_in_unit(Manager::Launchd, "not a plist"),
        HubEnsure::Unrecognized(_)
    ));
    assert!(matches!(
        ensure_hub_in_unit(Manager::Systemd, "[Unit]\nDescription=no exec\n"),
        HubEnsure::Unrecognized(_)
    ));
}

/// The reconcile probes the socket the unit pins, through the renderers'
/// escaping.
#[test]
fn the_socket_probed_is_the_one_the_unit_pins() {
    let custom = Some(PathBuf::from("/tmp/custom/phux.sock"));
    assert_eq!(unit_socket_override(Manager::Launchd, LEGACY_PLIST), custom);
    assert_eq!(unit_socket_override(Manager::Systemd, LEGACY_UNIT), custom);
    for socket in [None, Some(PathBuf::from("/tmp/100%/$HOME/a&b/phux.sock"))] {
        let mut plan = plan();
        plan.socket.clone_from(&socket);
        for manager in [Manager::Launchd, Manager::Systemd] {
            let body = render_unit(manager, &plan);
            assert_eq!(unit_socket_override(manager, &body), socket, "{manager:?}");
        }
    }
}

/// The installer's ambient `PATH` is never frozen into the unit; the init
/// system supplies its own.
#[test]
fn install_never_captures_the_process_path() {
    let ambient_path = std::env::var("PATH").unwrap_or_default();
    assert!(
        !ambient_path.is_empty(),
        "test process has no PATH; this assertion would be vacuous"
    );

    let plan = resolve_plan(None, None, false, None, false).expect("resolve_plan");
    assert!(
        plan.environment().iter().all(|(key, _)| *key != "PATH"),
        "the generated unit's environment must never carry a PATH key at all — \
         the init system supplies its own"
    );
    for manager in [Manager::Launchd, Manager::Systemd] {
        assert!(
            !render_unit(manager, &plan).contains(&ambient_path),
            "{manager:?}"
        );
    }
}

/// Only the unit that owns this socket completes a pending adoption
/// (ADR-0088), checked against really rendered units.
#[test]
fn only_the_unit_that_owns_this_socket_completes_an_adoption() {
    let overridden = PathBuf::from("/tmp/custom/phux.sock");
    let plan = resolve_plan(None, None, false, Some(overridden.clone()), false)
        .expect("resolve_plan with a socket override");

    for manager in [Manager::Launchd, Manager::Systemd] {
        let body = render_unit(manager, &plan);
        assert!(
            unit_supervises(manager, &body, &overridden),
            "{manager:?}: a unit carrying PHUX_SOCKET must match that socket"
        );
        assert!(
            !unit_supervises(manager, &body, &PathBuf::from("/tmp/somewhere-else.sock")),
            "{manager:?}: a unit for another socket must not complete this adoption"
        );
    }

    let default = resolve_plan(None, None, false, None, false).expect("resolve_plan");
    for manager in [Manager::Launchd, Manager::Systemd] {
        let body = render_unit(manager, &default);
        assert!(
            unit_supervises(manager, &body, &default.socket_path),
            "{manager:?}: a default-socket unit must match the resolved default"
        );
        assert!(
            !unit_supervises(manager, &body, &overridden),
            "{manager:?}: a default-socket unit must not claim an overridden socket"
        );
    }
}

/// Arming never starts anything; on launchd it runs no command, and the
/// written plist still carries `RunAtLoad`.
#[test]
fn arming_a_launchd_unit_runs_no_command() {
    arm_unit(Manager::Launchd)
        .expect("arming a launchd unit is writing the file, which the caller already did");

    let plan = resolve_plan(None, None, false, None, false).expect("resolve_plan");
    assert!(
        render_launchd_plist(&plan).contains("<key>RunAtLoad</key>\n  <true/>"),
        "an armed plist must still start the server at bootstrap"
    );
}
