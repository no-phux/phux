//! Signature, pin, rollback, channel, and handoff refusals.

use super::*;

/// Run a default update against `fake` and return the refusal, checking
/// that the install was left alone.
fn refused(fake: &FakeReleases, bin: &Path) -> UpdateError {
    let err = run(fake, direct(&bin.join("phux")), Handoff::Skipped, &opts()).unwrap_err();
    assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
    assert!(!bin.join(BACKUP_DIR).exists());
    err
}

fn signature_refusal(err: &UpdateError, expect: &str) {
    assert!(matches!(err, UpdateError::Signature(_)), "{err:?}");
    assert_eq!(err.code(), "update_signature_invalid");
    assert_eq!(err.exit_code(), 1);
    assert!(err.to_string().contains(expect), "{err}");
}

/// A missing signature, one from another key, and one over different
/// bytes each refuse before anything is unpacked.
#[test]
fn an_unsigned_foreign_or_tampered_checksum_installs_nothing() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = seed_bin(scratch.path());
    let mut fake = FakeReleases::new("v9.9.9");
    let artifact = publish(&mut fake, scratch.path(), "v9.9.9");
    let signed = String::from_utf8(fake.files[&artifact.sidecar_url()].clone()).unwrap();
    let good = fake.files[&artifact.signature_url()].clone();

    fake.files.remove(&artifact.signature_url());
    signature_refusal(&refused(&fake, &bin), "never installed");

    sign(&mut fake, &artifact, &signed, &TestKey::new(99));
    signature_refusal(&refused(&fake, &bin), "does not trust");

    // A rewritten sidecar under the genuine signature.
    let forged = format!("{}  {}\n", "e".repeat(64), artifact.name);
    fake.serve(&artifact.sidecar_url(), forged.into_bytes());
    fake.serve(&artifact.signature_url(), good);
    signature_refusal(&refused(&fake, &bin), "does not verify");
}

/// A host that rewrites the manifest and the sidecar together still
/// cannot sign; one that only rewrites the manifest is caught by the
/// signed digest.
#[test]
fn a_manifest_that_disagrees_with_the_signed_checksum_is_refused() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = seed_bin(scratch.path());
    let mut fake = FakeReleases::new("v9.9.9");
    publish(&mut fake, scratch.path(), "v9.9.9");
    fake.name(Channel::Stable, "9.9.9", &"c".repeat(40), &"f".repeat(64));
    signature_refusal(&refused(&fake, &bin), "the signed checksum is");
}

/// A stable manifest rewritten to name an older, genuinely signed release
/// does not roll a plain `phux update` back: nothing is fetched or
/// installed. A `--version` pin to the same release still installs it.
#[test]
fn a_manifest_naming_an_older_signed_release_needs_a_pin() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = seed_bin(scratch.path());
    let mut fake = FakeReleases::new("v0.0.1");
    publish(&mut fake, scratch.path(), "v0.0.1");

    let err = refused(&fake, &bin);
    assert!(matches!(err, UpdateError::Downgrade(_)), "{err:?}");
    assert_eq!(err.code(), "update_downgrade_refused");
    assert!(err.to_string().contains("v0.0.1"), "{err}");
    assert!(err.remedy().contains("--version"), "{}", err.remedy());
    assert!(fake.downloads.borrow().is_empty());

    let pinned = UpdateOpts {
        tag: Some("v0.0.1".to_owned()),
        ..opts()
    };
    let outcome = run(&fake, direct(&bin.join("phux")), Handoff::Skipped, &pinned).unwrap();
    assert_eq!(outcome.action, Action::Installed);
    assert_eq!(fs::read(bin.join("phux")).unwrap(), b"phux v0.0.1");
}

/// A pin to a release the manifest does not name is verified against that
/// release's own sidecar, and still works when the manifest is gone.
#[test]
fn a_pin_verifies_its_sidecar_and_survives_a_missing_manifest() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = seed_bin(scratch.path());
    let mut fake = FakeReleases::new("v9.9.9");
    let (pinned, _) = publish_artifact(
        &mut fake,
        scratch.path(),
        &Build::Stable(version("9.9.8")),
        "v9.9.8",
    );
    let pin = UpdateOpts {
        tag: Some("v9.9.8".to_owned()),
        ..opts()
    };

    let outcome = run(&fake, direct(&bin.join("phux")), Handoff::Skipped, &pin).unwrap();
    assert_eq!(outcome.action, Action::Installed);
    assert_eq!(outcome.plan.latest_tag(), "v9.9.9");
    assert_eq!(fs::read(bin.join("phux")).unwrap(), b"phux v9.9.8");
    assert!(fake.downloads.borrow().contains(&pinned.sidecar_url()));

    fake.manifests.clear();
    let outcome = run(
        &fake,
        direct(&bin.join("phux")),
        Handoff::Skipped,
        &UpdateOpts {
            dry_run: true,
            ..pin
        },
    )
    .unwrap();
    assert_eq!(outcome.action, Action::Planned);
    assert_eq!(outcome.plan.latest_tag(), "v9.9.8");

    let err = run(&fake, direct(&bin.join("phux")), Handoff::Skipped, &opts()).unwrap_err();
    assert!(matches!(err, UpdateError::Fetch(_)), "{err:?}");
}

/// A tampered archive and a missing artifact both fail without touching
/// the install.
#[test]
fn a_tampered_or_missing_archive_is_refused_and_nothing_is_replaced() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = seed_bin(scratch.path());
    let mut fake = FakeReleases::new("v9.9.9");
    let artifact = publish(&mut fake, scratch.path(), "v9.9.9");
    // Swap the archive after the sidecar was published.
    fake.serve(&artifact.url, b"totally different bytes".to_vec());

    let err = run(&fake, direct(&bin.join("phux")), Handoff::Skipped, &opts()).unwrap_err();
    assert!(
        matches!(err, UpdateError::ChecksumMismatch { .. }),
        "{err:?}"
    );
    assert_eq!(err.exit_code(), 1);
    assert_eq!(err.code(), "update_checksum_mismatch");
    assert!(err.to_string().contains("checksum mismatch"));
    assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
    assert!(!bin.join(BACKUP_DIR).exists());

    let empty = FakeReleases::new("v9.9.9");
    let err = run(&empty, direct(&bin.join("phux")), Handoff::Skipped, &opts()).unwrap_err();
    assert!(matches!(err, UpdateError::Fetch(_)), "{err:?}");
    assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
}

#[test]
fn an_install_already_on_the_target_release_does_nothing() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = seed_bin(scratch.path());
    let fake = FakeReleases::new(&format!("v{}", current_version().unwrap()));

    let outcome = run(&fake, direct(&bin.join("phux")), Handoff::Skipped, &opts()).unwrap();
    assert_eq!(outcome.action, Action::UpToDate);
    assert!(!outcome.plan.update_available);
    assert!(fake.downloads.borrow().is_empty());
    assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
}

#[test]
fn rollback_restores_the_previous_release_and_hands_off_again() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = seed_bin(scratch.path());
    let mut fake = FakeReleases::new("v9.9.9");
    publish(&mut fake, scratch.path(), "v9.9.9");
    let install = direct(&bin.join("phux"));
    let rollback = UpdateOpts {
        rollback: true,
        ..opts()
    };

    run(&fake, install.clone(), Handoff::Upgrading, &opts()).unwrap();
    assert_eq!(fs::read(bin.join("phux")).unwrap(), b"phux v9.9.9");

    let outcome = run(&fake, install.clone(), Handoff::Upgrading, &rollback).unwrap();
    assert_eq!(outcome.action, Action::RolledBack);
    assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
    assert_eq!(fs::read(bin.join("phux-mcp")).unwrap(), b"old phux-mcp");
    assert_eq!(outcome.document()["action"], "rolled-back");
    // `--no-restart` leaves the server alone on the way back too.
    assert_eq!(outcome.handoff, Some(Handoff::Skipped));
    assert_eq!(outcome.document()["server_handoff"]["result"], "skipped");

    // Without it, the rollback hands the live server back.
    run(&fake, install.clone(), Handoff::Upgrading, &opts()).unwrap();
    let outcome = run(
        &fake,
        install.clone(),
        Handoff::Upgrading,
        &UpdateOpts {
            no_restart: false,
            ..rollback
        },
    )
    .unwrap();
    assert_eq!(outcome.handoff, Some(Handoff::Upgrading));

    // Nothing left to restore.
    let err = run(
        &fake,
        install,
        Handoff::Upgrading,
        &UpdateOpts {
            rollback: true,
            ..opts()
        },
    )
    .unwrap_err();
    assert!(matches!(err, UpdateError::NoBackup(_)), "{err:?}");
    assert_eq!(err.code(), "update_no_backup");
}

#[test]
fn next_channel_and_a_stable_tag_are_refused_together() {
    let err = run(
        &FakeReleases::new("v0.13.0"),
        direct(Path::new("/home/ada/.local/bin/phux")),
        Handoff::Skipped,
        &UpdateOpts {
            channel: Some(ChannelArg::Next),
            tag: Some("v0.13.0".to_owned()),
            ..check()
        },
    )
    .unwrap_err();
    assert!(matches!(err, UpdateError::InvalidTag(_)), "{err:?}");
}

#[test]
fn check_on_next_does_not_download_an_archive() {
    let mut fake = FakeReleases::new("v0.13.0");
    fake.name(Channel::Next, "0.13.0", NEXT_SHA, UNUSED_DIGEST);
    let outcome = run(
        &fake,
        direct(Path::new("/home/ada/.local/bin/phux")),
        Handoff::Skipped,
        &UpdateOpts {
            channel: Some(ChannelArg::Next),
            ..check()
        },
    )
    .unwrap();
    assert_eq!(outcome.action, Action::Checked);
    assert_eq!(outcome.plan.channel, Channel::Next);
    assert_eq!(outcome.plan.latest_sha(), Some(NEXT_SHA));
    assert!(outcome.plan.update_available);
    assert!(fake.downloads.borrow().is_empty());
    assert_eq!(outcome.document()["channel"], "next");
    assert_eq!(outcome.document()["latest_sha"], NEXT_SHA);
}

#[test]
fn a_next_update_installs_and_persists_the_channel() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = seed_bin(scratch.path());
    let mut fake = FakeReleases::new("v0.13.0");
    let artifact = publish_next(&mut fake, scratch.path(), NEXT_SHA);
    let outcome = run(
        &fake,
        direct(&bin.join("phux")),
        Handoff::Skipped,
        &UpdateOpts {
            channel: Some(ChannelArg::Next),
            ..opts()
        },
    )
    .unwrap();
    assert_eq!(outcome.action, Action::Installed);
    assert_eq!(
        fs::read(bin.join("phux")).unwrap(),
        format!("phux {NEXT_SHA}").as_bytes()
    );
    assert_eq!(channel::read_file(&bin), Some(Channel::Next));
    let fetched = fake.downloads.borrow().clone();
    assert_eq!(
        fetched,
        vec![
            artifact.url.clone(),
            artifact.sidecar_url(),
            artifact.signature_url()
        ]
    );
}

#[test]
fn a_next_plan_is_up_to_date_only_on_the_same_sha() {
    let plan = |sha: &str| {
        plan_for(
            &Build::Next(sha.to_owned()),
            Channel::Next,
            "0.13.0",
            NEXT_SHA,
            None,
        )
        .unwrap()
    };
    let current = plan(NEXT_SHA);
    assert!(!current.update_available && !current.changes_version);
    assert!(plan(&"a".repeat(40)).update_available);
    assert!(
        plan_for(
            &Build::Stable(version("0.13.0")),
            Channel::Next,
            "0.13.0",
            NEXT_SHA,
            None
        )
        .unwrap()
        .update_available
    );
}

/// Every failure carries a code from the closed vocabulary, a non-empty
/// remedy, and one of the documented exit codes.
#[test]
fn every_failure_is_loud() {
    let failures = [
        UpdateError::InvalidTag("t".to_owned()),
        UpdateError::UnsupportedPlatform("p".to_owned()),
        UpdateError::Fetch("f".to_owned()),
        UpdateError::Checksum("c".to_owned()),
        UpdateError::ChecksumMismatch {
            expected: "a".to_owned(),
            actual: "b".to_owned(),
            archive: "x.tar.gz".to_owned(),
        },
        UpdateError::Archive("a".to_owned()),
        UpdateError::Install("i".to_owned()),
        UpdateError::NoBackup("n".to_owned()),
    ];
    for failure in &failures {
        assert!(failure.code().starts_with("update_"), "{failure:?}");
        assert!(!failure.to_string().is_empty(), "{failure:?}");
        assert!(!failure.remedy().is_empty(), "{failure:?}");
        assert!(
            matches!(failure.exit_code(), 1 | 2),
            "{failure:?} uses an undocumented exit code"
        );
    }
}

/// The handoff tokens are the frozen `--json` vocabulary.
#[test]
fn handoff_tokens_are_stable() {
    assert_eq!(Handoff::Upgrading.as_str(), "upgrading");
    assert_eq!(Handoff::NoServer.as_str(), "no_server");
    assert_eq!(Handoff::Skipped.as_str(), "skipped");
    assert_eq!(Handoff::Refused(String::new()).as_str(), "refused");
    assert_eq!(Handoff::Failed(String::new()).as_str(), "failed");
    assert_eq!(Handoff::Refused("why".to_owned()).detail(), Some("why"));
    assert_eq!(Handoff::Upgrading.detail(), None);
}

#[test]
fn a_stale_phux_earlier_on_path_is_named() {
    let temp = tempfile::tempdir().unwrap();
    let stale = temp.path().join("cargo-bin");
    let ours = temp.path().join("local-bin");
    for dir in [&stale, &ours] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("phux"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(dir.join("phux"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let install = direct(&ours.join("phux"));
    let path = std::env::join_paths([&stale, &ours]).unwrap();
    assert_eq!(
        super::super::shadowing_phux(&install, &path),
        Some(stale.join("phux"))
    );
    let path = std::env::join_paths([&ours, &stale]).unwrap();
    assert_eq!(super::super::shadowing_phux(&install, &path), None);
    let brew = install_at(&ours.join("phux"), InstallSource::Homebrew);
    let path = std::env::join_paths([&stale, &ours]).unwrap();
    assert_eq!(super::super::shadowing_phux(&brew, &path), None);
}

/// The desktop links the protocol in-process, and a server refuses a
/// client whose protocol `major.minor` differs. Its release train only
/// cuts a release for commits under its own directory, so a protocol bump
/// must edit the pin in the same commit to release the matching app.
#[test]
fn desktop_release_train_pins_the_protocol_it_links() {
    let protocol = format!(
        "{}.{}",
        phux_protocol::PROTOCOL_VERSION.major,
        phux_protocol::PROTOCOL_VERSION.minor
    );
    let pin = include_str!("../../../../../../clients/desktop/phux-protocol");
    assert_eq!(
        pin.trim(),
        protocol,
        "protocol is {protocol}: set clients/desktop/phux-protocol to {protocol} in this \
         commit so the app that links it is released with it"
    );
}
