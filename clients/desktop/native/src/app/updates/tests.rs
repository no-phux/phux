use super::*;

mod card;
mod restart;

fn release(version: &str, protocol: &str) -> Release {
    Release {
        version: version.into(),
        protocol: protocol.into(),
        ..Release::default()
    }
}

/// An installed desktop with shipped preferences, not the user's config.
fn installed(version: &str) -> Updater {
    let mut updater = Updater::new();
    updater.prefs = Preferences::default();
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

fn staged(version: &str, protocol: &str) -> Staged {
    Staged {
        app: "/Applications/.phux-desktop-update/Phux.app".into(),
        release: release(version, protocol),
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
    assert_eq!(updater.runtime_status().0, "Failed");
    updater.record_check(Err(anyhow::anyhow!("rate limited")), None);
    assert!(updater.target.is_none());
    assert!(updater.runtime.is_none());
    assert!(!updater.desktop_available());
    assert_eq!(updater.summary().0, "Could not check for updates");
}

#[cfg(target_os = "macos")]
#[test]
fn a_combined_download_prepares_desktop_but_leaves_the_server_running() {
    let mut updater = installed("0.2.0");
    let protocol = updates::own_protocol();
    updater.record_check(Ok(release("0.3.0", &protocol)), Some(Ok(runtime(None))));
    assert_eq!(updater.action(), Some(UpdateAction::Both));
    assert_eq!(UpdateAction::Both.label(), "Download Desktop");
    // Downloading never hands off the server or restarts Desktop.
    updater.busy = true;
    assert_eq!(updater.action(), None);
    updater.record_stage(Ok(staged("0.3.0", &protocol)));
    assert_eq!(updater.action(), Some(UpdateAction::Both));
    assert_eq!(updater.action_label(), Some("Restart & update server"));
    assert_eq!(updater.runtime.as_ref().unwrap().current, "1.0.0");
    assert!(!updater.relaunch);
    // The same protocol needs no warning: phux moves with the explicit restart.
    assert!(updater.notices().is_empty());
    updater.record_runtime(Ok(updates::RuntimeInstalled {
        socket: PathBuf::new(),
        message: String::new(),
        reconnect: true,
        retry_handoff: false,
    }));
    assert_eq!(
        updater.action(),
        Some(UpdateAction::Desktop { staged: true })
    );
    assert!(
        !updater.relaunch,
        "the combined update must wait for Restart now"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_desktop_only_download_waits_for_an_explicit_restart() {
    let mut updater = installed("0.2.0");
    updater.prefs.manage_runtime = false;
    let protocol = updates::own_protocol();
    updater.record_check(Ok(release("0.3.0", &protocol)), None);
    assert_eq!(
        updater.action(),
        Some(UpdateAction::Desktop { staged: false })
    );
    assert_eq!(updater.action().unwrap().label(), "Download update");
    // A download stays staged for quit or explicit restart.
    updater.record_stage(Ok(staged("0.3.0", &protocol)));
    assert_eq!(
        updater.action(),
        Some(UpdateAction::Desktop { staged: true })
    );
    assert_eq!(updater.action().unwrap().label(), "Restart now");
    assert!(!updater.relaunch, "downloading must not request a restart");
    assert_eq!(updater.desktop_status().0, "Downloaded");
    // A check after staging keeps the staged bundle.
    updater.record_check(Ok(release("0.3.0", &protocol)), None);
    assert_eq!(
        updater.action(),
        Some(UpdateAction::Desktop { staged: true })
    );
}

#[test]
fn a_phux_only_update_hands_off_without_a_restart() {
    let mut updater = installed("0.3.0");
    updater.record_check(
        Ok(release("0.3.0", &updates::own_protocol())),
        Some(Ok(runtime(None))),
    );
    assert_eq!(updater.action(), Some(UpdateAction::Phux { retry: false }));
    assert_eq!(updater.runtime_status().0, "Update ready");
    let installed = updates::RuntimeInstalled {
        socket: PathBuf::from("/tmp/first-launch-resolved.sock"),
        message: String::new(),
        reconnect: true,
        retry_handoff: false,
    };
    assert!(updater.record_runtime(Ok(installed)));
    assert_eq!(updater.runtime_generation, 1);
    assert_eq!(
        updater.local_socket(),
        PathBuf::from("/tmp/first-launch-resolved.sock")
    );
    let runtime = updater.runtime.as_ref().unwrap();
    assert_eq!(
        (runtime.current.as_str(), runtime.available),
        ("2.0.0", false)
    );
    assert_eq!(updater.runtime_status().0, "Up to date");
    assert_eq!(updater.action(), None);
    assert_eq!(updater.summary().0, "Local server updated");
}

#[test]
fn a_refused_handoff_offers_the_retry_and_keeps_the_desktop_staged() {
    let mut updater = installed("0.3.0");
    updater.runtime = Some(runtime(None));
    updater.staged = Some(staged("0.4.0", &updates::own_protocol()));
    let refused = updates::RuntimeInstalled {
        socket: PathBuf::new(),
        message: "phux installed. Server handoff: refused.".into(),
        reconnect: false,
        retry_handoff: true,
    };
    // No reconnect means no restart onto a server that did not move.
    assert!(!updater.record_runtime(Ok(refused)));
    assert!(updater.pending_handoff);
    assert!(updater.staged.is_some());
    assert_eq!(updater.runtime_status().0, "Handoff pending");
    let notices = updater.notices();
    assert!(
        notices
            .iter()
            .any(|(text, failed)| !failed && text.contains("refused")),
        "{notices:?}"
    );
    updater.staged = None;
    assert_eq!(updater.action(), Some(UpdateAction::Phux { retry: true }));
    assert_eq!(updater.action().unwrap().label(), "Retry server handoff");
}

#[test]
fn a_failed_phux_install_is_a_phux_failure() {
    let mut updater = installed("0.3.0");
    updater.runtime = Some(runtime(None));
    assert!(!updater.record_runtime(Err(anyhow::anyhow!("checksum mismatch"))));
    assert_eq!(updater.runtime_status().0, "Failed");
    assert_eq!(updater.summary().0, "The update did not finish");
    assert_eq!(
        updater.notices(),
        vec![("phux: Update failed: checksum mismatch".to_owned(), true)]
    );
}

#[test]
fn a_failed_stage_is_shown_as_a_desktop_failure() {
    let mut updater = installed("0.2.0");
    updater.record_check(Ok(release("0.3.0", "0.9")), None);
    updater.record_stage(Err(anyhow::anyhow!("checksum mismatch")));
    assert!(updater.staged.is_none());
    assert_eq!(updater.desktop_status().0, "Failed");
    // Failures lead; Linux follows with its installer command.
    assert_eq!(
        updater.notices().first(),
        Some(&("Desktop: Update failed: checksum mismatch".to_owned(), true))
    );
}

#[test]
fn switching_the_desktop_channel_drops_a_staged_update() {
    let mut updater = installed("0.2.0");
    updater.staged = Some(staged("0.3.0", "0.9"));
    let mut prefs = updater.prefs.clone();
    prefs.desktop = updates::Channel::Next;
    updater.adopt_preferences(prefs);
    assert!(updater.staged.is_none());
}

#[test]
fn a_protocol_change_is_named_until_phux_lands_on_it() {
    let mut updater = installed("0.2.0");
    updater.prefs.manage_runtime = false;
    updater.record_check(Ok(release("0.3.0", "99.0")), None);
    let (now, next) = updater.protocol_change().unwrap();
    assert_eq!(
        (now.as_str(), next.as_str()),
        (updates::own_protocol().as_str(), "99.0")
    );
    let warned = |updater: &Updater| {
        updater
            .notices()
            .iter()
            .any(|(text, _)| text.contains("wire protocol 99.0"))
    };
    assert!(warned(&updater));
    updater.prefs.manage_runtime = true;
    updater.runtime = Some(runtime(None));
    assert!(!warned(&updater), "managed phux moves with the update");
    // A runtime update that lands elsewhere cannot fix the mismatch.
    let mut elsewhere = runtime(None);
    elsewhere.protocol_target = Some("98.0".into());
    updater.runtime = Some(elsewhere);
    assert!(warned(&updater));
    // A server already on the incoming protocol has nothing to warn about.
    updater.runtime = Some(runtime(Some("99.0")));
    assert_eq!(updater.protocol_change(), None);
    assert!(!warned(&updater));
}

#[test]
fn a_package_managed_phux_names_its_own_command() {
    let mut updater = installed("0.3.0");
    let mut packaged = runtime(None);
    packaged.mutable = false;
    packaged.native_command = Some("brew upgrade phux".into());
    updater.runtime = Some(packaged);
    assert_eq!(updater.action(), None);
    assert_eq!(
        updater.runtime_status().0,
        if cfg!(windows) {
            "Manual in WSL"
        } else {
            "Package managed"
        }
    );
    assert!(
        updater
            .notices()
            .iter()
            .any(|(text, failed)| !failed && text.ends_with("brew upgrade phux"))
    );
}

#[cfg(windows)]
#[test]
fn windows_runtime_updates_show_manual_instructions_without_an_install_action() {
    let mut updater = installed("0.3.0");
    let mut readonly = runtime(None);
    readonly.mutable = false;
    readonly.native_command = Some("& 'wsl.exe' '--distribution' 'Ubuntu' '--exec' 'phux' '--socket' '/run/phux.sock' 'update' '--channel' 'stable'".into());
    updater.record_check(
        Ok(release("0.3.0", &updates::own_protocol())),
        Some(Ok(readonly)),
    );
    assert_eq!(updater.action(), None);
    assert!(!updater.phux_due());
    assert_eq!(updater.runtime_status().0, "Manual in WSL");
    assert!(
        updater
            .summary_detail()
            .unwrap()
            .contains("never starts a WSL install or server handoff")
    );
    assert!(updater.notices().iter().any(|(text, failed)| !failed
        && text.contains("Desktop did not start an install or server handoff")
        && text.contains("'--distribution' 'Ubuntu'")));
}

#[test]
fn progress_names_each_products_step() {
    let mut updater = installed("0.2.0");
    updater.runtime = Some(runtime(None));
    updater.target = Some(release("0.3.0", &updates::own_protocol()));
    updater.busy = true;
    updater.work = Work::Downloading;
    assert_eq!(updater.desktop_status().0, "Downloading…");
    assert_eq!(updater.runtime_status().0, "On restart");
    updater.staged = Some(staged("0.3.0", &updates::own_protocol()));
    updater.work = Work::Installing;
    assert_eq!(updater.desktop_status().0, "Restarts next");
    assert_eq!(updater.runtime_status().0, "Installing…");
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
    assert_eq!(updater.unresolved_protocol(), None, "nothing to warn about");
}

#[test]
fn same_protocol_line_compares_majors() {
    assert!(same_protocol_line("1.0", "1.7"));
    assert!(!same_protocol_line("0.9", "1.0"));
    assert!(!same_protocol_line("", "1.0"), "unknown shape is a change");
}

#[test]
fn card_is_quiet_until_there_is_something_to_do() {
    let mut updater = installed("0.3.0");
    assert!(!updater.has_card());
    updater.busy = true;
    updater.work = Work::Checking;
    assert!(
        !updater.has_card(),
        "automatic checks do not interrupt work"
    );
    updater.busy = false;
    updater.target = Some(release("0.4.0", &updates::own_protocol()));
    assert!(updater.has_card());
    updater.development = true;
    assert!(
        !updater.has_card(),
        "development builds cannot offer installation"
    );
}

#[test]
fn transfer_completion_is_not_verification_and_does_not_leak_into_the_next_step() {
    let mut updater = installed("0.2.0");
    updater.busy = true;
    updater.work = Work::Downloading;
    updater.progress = Some(updates::StageProgress::Downloading(100));
    assert!(updater.card_headline().contains("100%"));
    assert!(updater.staged.is_none());
    updater.progress = Some(updates::StageProgress::Verifying);
    assert_eq!(updater.card_headline(), "Verifying Desktop…");
    updater.record_stage(Err(anyhow::anyhow!("invalid signature")));
    assert!(updater.progress.is_none());
    assert!(updater.has_card(), "failure stays visible and retryable");
}

#[test]
fn progress_poll_coalesces_events_and_detects_the_finished_worker() {
    let (sender, receiver) = std::sync::mpsc::channel();
    sender
        .send(updates::StageProgress::Downloading(10))
        .unwrap();
    sender
        .send(updates::StageProgress::Downloading(70))
        .unwrap();
    assert_eq!(
        drain_progress(&receiver),
        (Some(updates::StageProgress::Downloading(70)), false)
    );
    sender.send(updates::StageProgress::Verifying).unwrap();
    drop(sender);
    assert_eq!(
        drain_progress(&receiver),
        (Some(updates::StageProgress::Verifying), true)
    );
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
        assert_eq!(updater.action(), None);
        updater.act(cx);
        assert!(!updater.busy);
        updater.development = false;
        updater.prefs.manage_runtime = false;
        assert_eq!(updater.action(), None);
        updater.act(cx);
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
        assert_eq!(updater.action(), None);
        updater.act(cx);
        assert!(!updater.busy);
        updater.staged = Some(staged("0.3.0", "0.9"));
        updater.apply_staged();
        assert!(!updater.handed_off);
    });
}

#[cfg(any(target_os = "linux", windows))]
#[gpui::test]
fn manual_install_action_never_stages(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;
    let updater = cx.new(|_| installed("0.2.0"));
    updater.update(cx, |updater, cx| {
        updater.prefs.manage_runtime = false;
        updater.target = Some(release("0.3.0", &updates::own_protocol()));
        updater.act(cx);
        assert!(!updater.busy);
        assert!(updater.staged.is_none());
        assert!(!updater.handed_off);
        assert_eq!(updater.action_label(), Some("Installation instructions"));
        assert!(updater.summary_detail().unwrap().contains("rerun"));
        assert_eq!(
            updater.message,
            updates::install_remedy(updater.prefs.desktop, None)
        );
    });
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn manual_install_platforms_move_phux_alone_and_name_the_desktop_installer() {
    let mut updater = installed("0.2.0");
    updater.record_check(
        Ok(release("0.3.0", &updates::own_protocol())),
        Some(Ok(runtime(None))),
    );
    assert_eq!(updater.action(), Some(UpdateAction::Phux { retry: false }));
    assert!(
        updater
            .notices()
            .iter()
            .any(|(text, _)| text == &updates::install_remedy(updater.prefs.desktop, None))
    );
}
