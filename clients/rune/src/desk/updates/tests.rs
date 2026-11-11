use super::*;

fn updater() -> Updater {
    Updater {
        automatic: true,
        manage_phux: true,
        phux_channel: None,
        install: Ok(PathBuf::from("/Applications/Phux Rune.app")),
        target: Some(("/phux".into(), "/sock".into())),
        aimed: false,
        work: Work::Idle,
        release: None,
        cli: None,
        staged: None,
        pending_handoff: false,
        message: None,
        asked: false,
        dismissed: None,
        copied: false,
        done: channel(),
        waker: None,
    }
}

fn release(protocol: &str) -> Release {
    Release {
        version: "99.0.0".to_owned(),
        sha256: String::new(),
        size: 1,
        protocol: protocol.to_owned(),
    }
}

fn cli(target_protocol: &str) -> Cli {
    Cli {
        current: "1.0.0".to_owned(),
        target: "2.0.0".to_owned(),
        available: true,
        mutable: true,
        native_command: None,
        channel: Some(Channel::Stable),
        protocol_current: Some("1.0".to_owned()),
        protocol_target: Some(target_protocol.to_owned()),
    }
}

#[test]
fn changing_channels_discards_the_previous_target_even_if_the_recheck_fails() {
    let mut updater = updater();
    updater.cli = Some(cli("1.0"));
    updater.release = Some(release("1.0"));
    assert!(updater.choose_phux_channel(Some(Channel::Next)));
    assert!(updater.cli.is_none());
    assert!(updater.release.is_some(), "Rune's app feed is independent");
    updater.checked(Err("offline".into()), Some(Err("offline".into())));
    assert!(!updater.phux_due());
    assert_eq!(updater.action(), Some(Action::Download));
}

#[test]
fn channels_cannot_change_during_work_or_a_pending_update() {
    let mut updater = updater();
    for work in [
        Work::Checking,
        Work::Downloading(2),
        Work::Verifying,
        Work::Server,
    ] {
        updater.work = work;
        assert!(!updater.choose_phux_channel(Some(Channel::Next)));
    }
    updater.work = Work::Idle;
    updater.staged = Some("99.0.0".into());
    assert!(!updater.choose_phux_channel(Some(Channel::Next)));
    updater.staged = None;
    updater.pending_handoff = true;
    assert!(!updater.choose_phux_channel(Some(Channel::Next)));
    updater.pending_handoff = false;
    assert!(updater.choose_phux_channel(Some(Channel::Next)));
}

#[test]
fn unmanaged_and_explicit_installs_keep_their_channel_policy() {
    let mut updater = updater();
    updater.cli = Some(Cli {
        mutable: false,
        ..cli("1.0")
    });
    assert!(!updater.choose_phux_channel(Some(Channel::Next)));
    updater.cli = None;
    updater.manage_phux = false;
    assert!(!updater.choose_phux_channel(Some(Channel::Next)));
    updater.manage_phux = true;
    updater.target = None;
    assert!(!updater.choose_phux_channel(Some(Channel::Next)));
}

#[test]
fn channel_preferences_load_safely_and_preserve_the_installed_default() {
    let mut prefs = Prefs::memory();
    let resolved = Resolved::offline("channel prefs");
    assert_eq!(Updater::new(&prefs, &resolved).phux_channel, None);
    for (value, expected) in [
        ("next", Some(Channel::Next)),
        ("stable", Some(Channel::Stable)),
        ("installed", None),
        ("unknown", None),
    ] {
        prefs.set("update_phux_channel", &value).unwrap();
        assert_eq!(Updater::new(&prefs, &resolved).phux_channel, expected);
    }
}

#[test]
fn selecting_a_channel_persists_without_installing_anything() {
    let mut desk = Desk::new(&Resolved::offline("channel selection"));
    desk.prefs = Prefs::memory();
    desk.updater = updater();
    // Disable network and subprocesses while exercising the public action.
    desk.updater.install = Err("test copy: no network");
    for channel in [Some(Channel::Next), Some(Channel::Stable), None] {
        desk.set_phux_channel(channel);
        assert_eq!(
            Updater::new(&desk.prefs, &Resolved::offline("reload")).phux_channel,
            channel
        );
        assert_eq!(desk.updater.work, Work::Idle);
        assert!(desk.updater.staged.is_none());
    }
}

#[test]
fn settings_expose_the_local_cli_channel_and_external_ownership() {
    use rune::accesskit::Action as A11yAction;

    let mut desk = Desk::settings_test_fixture();
    desk.updater = updater();
    desk.updater.cli = Some(cli("1.0"));
    desk.settings_category(crate::desk::SettingsCategory::About);
    let mut rt = rune::Runtime::new(
        desk,
        crate::ui::view,
        (1100.0, 720.0),
        rune::Env::new(false, 1.0),
    );
    rt.frame(0.0);
    for key in [
        "phux-channel-installed",
        "phux-channel-stable",
        "phux-channel-next",
    ] {
        assert!(rt.find(key).is_some(), "{key}");
    }
    rt.update(|d, _| d.updater.work = Work::Checking);
    rt.frame(0.0);
    // Rune accepts the accessibility request even for a disabled control;
    // the controller must also refuse the change, not just dim the button.
    rt.accessibility_action("phux-channel-next", A11yAction::Click, None);
    assert_eq!(rt.state.updater.phux_channel, None);
    assert_eq!(rt.state.updater.work, Work::Checking);
    rt.update(|d, _| {
        d.updater.work = Work::Idle;
        d.updater.cli.as_mut().unwrap().mutable = false;
    });
    rt.frame(0.0);
    assert!(rt.find("phux-channel-next").is_none());
    assert!(!rt.state.updater.can_choose_phux_channel());
}

#[test]
fn channel_choices_fit_desktop_and_narrow_phone_settings() {
    for (size, phone) in [
        ((1100.0, 720.0), false),
        ((640.0, 480.0), false),
        ((320.0, 568.0), true),
    ] {
        let mut desk = Desk::settings_test_fixture().with_phone(phone);
        desk.updater = updater();
        desk.settings_category(crate::desk::SettingsCategory::About);
        let mut rt = rune::Runtime::new(desk, crate::ui::view, size, rune::Env::new(false, 1.0));
        rt.frame(0.0);
        for key in [
            "phux-channel-installed",
            "phux-channel-stable",
            "phux-channel-next",
        ] {
            let elements = rt.elements();
            let control = elements
                .iter()
                .find(|e| e.key.as_deref() == Some(key))
                .unwrap();
            let [x, _, width, height] = control.rect;
            assert!(
                x >= 0.0 && x + width <= size.0 && height >= 44.0,
                "{key}: {:?}",
                control.rect
            );
        }
    }
}

#[test]
fn management_toggles_cannot_discard_a_refused_handoff_or_unlock_channels() {
    let mut desk = Desk::new(&Resolved::offline("pending handoff"));
    desk.prefs = Prefs::memory();
    desk.updater = updater();
    desk.updater.cli = Some(cli("1.0"));
    assert!(!desk.updater.server_done(Ok(Handoff {
        done: false,
        retry: true,
        detail: "busy server".into(),
    })));
    // The public toggle actions must not provide an off/on escape hatch.
    desk.toggle_update_phux();
    desk.toggle_update_phux();
    desk.set_phux_channel(Some(Channel::Next));
    assert!(desk.updater.manage_phux);
    assert!(desk.updater.pending_handoff);
    assert_eq!(desk.updater.phux_channel, None);
    assert_eq!(
        desk.updater.action(),
        Some(Action::UpdateServer { retry: true })
    );
    assert_eq!(desk.prefs.get::<bool>("update_phux"), None);
    assert_eq!(desk.prefs.get::<String>("update_phux_channel"), None);
}

#[test]
fn management_cannot_change_beneath_work_or_a_staged_app() {
    let mut desk = Desk::new(&Resolved::offline("pending update"));
    desk.updater = updater();
    desk.updater.work = Work::Checking;
    desk.toggle_update_phux();
    assert!(desk.updater.manage_phux);
    desk.updater.work = Work::Idle;
    desk.updater.staged = Some("99.0.0".into());
    desk.toggle_update_phux();
    assert!(desk.updater.manage_phux);
}

#[test]
fn nothing_due_offers_nothing() {
    let mut updater = updater();
    assert_eq!(updater.action(), None);
    updater.release = Some(Release {
        version: "0.0.1".to_owned(),
        ..release("1.0")
    });
    assert_eq!(updater.action(), None);
}

#[test]
fn the_action_follows_what_is_due() {
    let mut updater = updater();
    updater.release = Some(release("1.0"));
    assert_eq!(updater.action(), Some(Action::Download));
    updater.cli = Some(cli("1.0"));
    assert_eq!(updater.action(), Some(Action::DownloadForBoth));
    updater.staged = Some("99.0.0".to_owned());
    assert_eq!(updater.action(), Some(Action::RestartAndUpdateServer));
    updater.cli = None;
    assert_eq!(updater.action(), Some(Action::Restart));
}

#[test]
fn a_restart_mid_reconnect_returns_to_the_session_it_was_on() {
    let mut desk = Desk::open(&Resolved::offline("not launched"), None);
    assert_eq!(desk.return_session(), None);
    desk.link.home = Some(("work".to_owned(), None));
    assert_eq!(desk.return_session().as_deref(), Some("work"));
}

#[test]
fn a_window_aimed_by_hand_quits_instead_of_relaunching() {
    let mut updater = updater();
    updater.target = None;
    updater.aimed = true;
    updater.staged = Some("99.0.0".to_owned());
    assert_eq!(updater.action(), Some(Action::Quit));
}

#[test]
fn server_only_and_its_retry() {
    let mut updater = updater();
    updater.cli = Some(cli("1.0"));
    assert_eq!(
        updater.action(),
        Some(Action::UpdateServer { retry: false })
    );
    assert!(!updater.server_done(Ok(Handoff {
        done: false,
        retry: true,
        detail: "agents are mid-turn".to_owned(),
    })));
    assert_eq!(updater.action(), Some(Action::UpdateServer { retry: true }));
    assert!(updater.message.as_deref().unwrap().contains("No session"));
}

#[test]
fn a_package_managed_cli_is_never_touched() {
    let mut updater = updater();
    updater.cli = Some(Cli {
        mutable: false,
        ..cli("1.0")
    });
    assert_eq!(updater.action(), None);
}

#[test]
fn a_window_aimed_by_hand_never_updates_phux() {
    let mut updater = updater();
    updater.target = None;
    updater.cli = Some(cli("1.0"));
    assert!(!updater.phux_due());
    assert_eq!(updater.action(), None);
}

#[test]
fn a_development_build_updates_nothing() {
    let mut updater = updater();
    updater.install = Err("Development build");
    updater.release = Some(release("1.0"));
    assert_eq!(updater.action(), None);
    updater.check();
    assert_eq!(updater.work, Work::Idle);
}

#[test]
fn busy_work_hides_the_button() {
    let mut updater = updater();
    updater.release = Some(release("1.0"));
    updater.work = Work::Verifying;
    assert_eq!(updater.action(), None);
}

#[test]
fn a_protocol_change_warns_unless_the_server_lands_on_it() {
    let mut updater = updater();
    updater.release = Some(release("2.0"));
    updater.cli = Some(cli("1.0"));
    assert!(updater.protocol_warning().is_some());
    updater.cli = Some(cli("2.0"));
    assert_eq!(updater.protocol_warning(), None);
    updater.manage_phux = false;
    assert!(updater.protocol_warning().is_some());
}

#[test]
fn a_dismissed_card_stays_shut_until_a_newer_version() {
    let mut updater = updater();
    updater.release = Some(release("1.0"));
    assert!(updater.card_open());
    updater.dismiss();
    assert!(!updater.card_open());
    updater.release = Some(Release {
        version: "100.0.0".to_owned(),
        ..release("1.0")
    });
    assert!(updater.card_open());
}

#[test]
fn asking_always_answers_and_closing_ends_it() {
    let mut updater = updater();
    updater.check_now();
    assert!(updater.card_open());
    updater.checked(
        Ok(Release {
            version: "0.0.1".to_owned(),
            ..release("1.0")
        }),
        None,
    );
    assert_eq!(updater.action(), None);
    assert!(updater.card_open());
    updater.dismiss();
    assert!(!updater.card_open());
}

#[test]
fn a_background_check_stays_quiet_when_current() {
    let mut updater = updater();
    updater.checked(
        Ok(Release {
            version: "0.0.1".to_owned(),
            ..release("1.0")
        }),
        None,
    );
    assert!(!updater.card_open());
}

#[test]
fn a_failed_recheck_keeps_what_is_known() {
    let mut updater = updater();
    updater.release = Some(release("1.0"));
    updater.checked(Err("offline".to_owned()), None);
    assert_eq!(updater.action(), Some(Action::Download));
    assert!(updater.message.as_deref().unwrap().contains("offline"));
}

#[test]
fn the_copied_mark_ends_on_its_own() {
    let mut updater = updater();
    updater.show_copied(Duration::ZERO);
    assert!(updater.copied);
    let done = updater.done.1.recv_timeout(Duration::from_secs(5));
    assert!(matches!(done, Ok(Done::CopiedShown)));
}
