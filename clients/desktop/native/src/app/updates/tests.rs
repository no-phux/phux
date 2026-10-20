use super::*;

fn release(version: &str, protocol: &str) -> Release {
    Release {
        version: version.into(),
        protocol: protocol.into(),
        ..Release::default()
    }
}

fn installed(version: &str) -> Updater {
    let mut updater = Updater::new();
    updater.current = Some(release(version, &updates::own_protocol()));
    updater.development = false;
    updater
}

fn runtime(protocol_current: Option<&str>) -> RuntimeRelease {
    RuntimeRelease {
        current: "1.0.0".into(),
        target: "2.0.0".into(),
        available: true,
        mutable: true,
        native_command: None,
        protocol_current: protocol_current.map(str::to_owned),
        protocol_target: None,
    }
}

#[test]
fn a_failed_check_clears_stale_targets_but_preserves_independent_success() {
    let mut updater = installed("0.2.0");
    updater.busy = true;
    updater.record_check(
        Ok(release("0.3.0", "0.9")),
        Some(Err(anyhow::anyhow!("offline"))),
    );
    assert!(!updater.busy);
    assert!(updater.desktop_available());
    assert!(updater.message.contains("phux: offline"));
    updater.record_check(Err(anyhow::anyhow!("rate limited")), None);
    assert!(updater.target.is_none());
    assert!(updater.runtime.is_none());
    assert!(!updater.desktop_available());
}

#[test]
fn an_available_update_downloads_then_offers_restart() {
    let mut updater = installed("0.2.0");
    let protocol = updates::own_protocol();
    updater.record_check(Ok(release("0.3.0", &protocol)), None);
    assert_eq!(updater.desktop_action(), Some(DesktopAction::Download));
    updater.busy = true;
    assert_eq!(updater.desktop_action(), None);
    updater.record_stage(Ok(Staged {
        app: "/Applications/.phux-desktop-update/Phux.app".into(),
        release: release("0.3.0", &protocol),
    }));
    assert!(!updater.busy);
    assert_eq!(updater.desktop_action(), Some(DesktopAction::Restart));
    assert_eq!(DesktopAction::Restart.label(), "Restart to update");
    let (detail, failed) = updater.desktop_detail().unwrap();
    assert!(!failed);
    assert!(detail.contains("installs when you quit"), "{detail}");
    // A check after staging keeps the staged bundle.
    updater.record_check(Ok(release("0.3.0", &protocol)), None);
    assert_eq!(updater.desktop_action(), Some(DesktopAction::Restart));
}

#[test]
fn a_failed_stage_is_shown_as_a_desktop_failure() {
    let mut updater = installed("0.2.0");
    updater.record_check(Ok(release("0.3.0", "0.9")), None);
    updater.record_stage(Err(anyhow::anyhow!("checksum mismatch")));
    assert!(updater.staged.is_none());
    let (detail, failed) = updater.desktop_detail().unwrap();
    assert!(failed);
    assert_eq!(detail, "Update failed: checksum mismatch");
}

#[test]
fn switching_the_desktop_channel_drops_a_staged_update() {
    let mut updater = installed("0.2.0");
    updater.staged = Some(Staged {
        app: PathBuf::from("/x/Phux.app"),
        release: release("0.3.0", "0.9"),
    });
    // The loaded preferences come from the user's config; switch away
    // from whichever channel they hold.
    let mut prefs = updater.prefs.clone();
    prefs.desktop = if prefs.desktop == updates::Channel::Next {
        updates::Channel::Stable
    } else {
        updates::Channel::Next
    };
    updater.adopt_preferences(prefs);
    assert!(updater.staged.is_none());
}

#[test]
fn a_protocol_change_is_named_and_couples_a_managed_runtime() {
    let mut updater = installed("0.2.0");
    updater.record_check(Ok(release("0.3.0", "99.0")), None);
    let (now, next) = updater.protocol_change().unwrap();
    assert_eq!(
        (now.as_str(), next.as_str()),
        (updates::own_protocol().as_str(), "99.0")
    );
    let (detail, _) = updater.desktop_detail().unwrap();
    assert!(detail.contains("must be updated too"), "{detail}");
    updater.staged = Some(Staged {
        app: PathBuf::from("/x/Phux.app"),
        release: release("0.3.0", "99.0"),
    });
    assert_eq!(updater.desktop_action(), Some(DesktopAction::Restart));
    updater.prefs.manage_runtime = true;
    updater.runtime = Some(runtime(None));
    assert_eq!(
        updater.desktop_action(),
        Some(DesktopAction::RuntimeThenRestart)
    );
    // A runtime update that lands elsewhere cannot fix the mismatch.
    let mut elsewhere = runtime(None);
    elsewhere.protocol_target = Some("98.0".into());
    updater.runtime = Some(elsewhere);
    assert_eq!(updater.desktop_action(), Some(DesktopAction::Restart));
    // A server already on the incoming protocol needs no coupling.
    updater.runtime = Some(runtime(Some("99.0")));
    assert_eq!(updater.protocol_change(), None);
    assert_eq!(updater.desktop_action(), Some(DesktopAction::Restart));
}

#[test]
fn a_minor_protocol_difference_on_the_same_major_is_not_a_change() {
    let version = phux_protocol::PROTOCOL_VERSION;
    let newer_minor = format!("{}.{}", version.major, version.minor + 1);
    let mut updater = installed("0.2.0");
    updater.record_check(Ok(release("0.3.0", &newer_minor)), None);
    assert_eq!(updater.protocol_change(), None);
    updater.prefs.manage_runtime = true;
    let older_minor = format!("{}.{}", version.major, version.minor.wrapping_sub(1));
    updater.runtime = Some(runtime(Some(&older_minor)));
    assert_eq!(updater.protocol_change(), None);
    assert_eq!(updater.desktop_action(), Some(DesktopAction::Download));
}

#[test]
fn same_protocol_line_compares_majors() {
    assert!(same_protocol_line("1.0", "1.7"));
    assert!(!same_protocol_line("0.9", "1.0"));
    assert!(!same_protocol_line("", "1.0"), "unknown shape is a change");
}

#[test]
fn opting_out_does_not_start_another_check() {
    let mut before = Preferences::default();
    let mut after = before.clone();
    after.automatic = false;
    assert!(!needs_recheck(&before, &after));
    before.manage_runtime = true;
    after = before.clone();
    after.manage_runtime = false;
    assert!(!needs_recheck(&before, &after));
    after.desktop = updates::Channel::Next;
    assert!(needs_recheck(&before, &after));
}

#[gpui::test]
fn development_and_unmanaged_runtime_cannot_install(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;
    let updater = cx.new(|_| Updater::new());
    updater.update(cx, |updater, cx| {
        updater.prefs.manage_runtime = true;
        updater.runtime = Some(runtime(None));
        updater.install_runtime(cx);
        assert!(!updater.busy);
        updater.development = false;
        updater.prefs.manage_runtime = false;
        updater.install_runtime(cx);
        assert!(!updater.busy);
        updater.desktop_act(cx);
        assert!(!updater.busy);
    });
}

#[gpui::test]
fn development_builds_never_stage_or_swap(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;
    let updater = cx.new(|_| Updater::new());
    updater.update(cx, |updater, cx| {
        updater.current = Some(release("0.2.0", "0.9"));
        updater.target = Some(release("0.3.0", "0.9"));
        assert!(updater.development);
        assert_eq!(updater.desktop_action(), None);
        updater.desktop_act(cx);
        assert!(!updater.busy);
        updater.staged = Some(Staged {
            app: PathBuf::from("/x/Phux.app"),
            release: release("0.3.0", "0.9"),
        });
        updater.apply_staged();
        assert!(!updater.handed_off);
    });
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn linux_install_action_is_manual_and_never_stages(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;
    let updater = cx.new(|_| installed("0.2.0"));
    updater.update(cx, |updater, cx| {
        updater.target = Some(release("0.3.0", &updates::own_protocol()));
        updater.desktop_act(cx);
        assert!(!updater.busy);
        assert!(updater.staged.is_none());
        assert!(!updater.handed_off);
        assert!(updater.message.contains("PHUX_REQUIRE_SIGNATURE=1"));
    });
}
