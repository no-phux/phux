use super::*;

fn updater() -> Updater {
    Updater {
        automatic: true,
        manage_phux: true,
        install: Ok(PathBuf::from("/Applications/Phux Rune.app")),
        target: Some(("/phux".into(), "/sock".into())),
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
        protocol_current: Some("1.0".to_owned()),
        protocol_target: Some(target_protocol.to_owned()),
    }
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
