use super::*;

#[test]
fn changed_malformed_generation_denies_instead_of_using_cached_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let first = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let store = ReloadingTokenStore::load(path.clone()).unwrap();
    let second = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    assert!(store.verify(&secret(&second)));
    assert_eq!(store.reloads(), 1);
    fs::write(&path, "{broken").unwrap();
    assert!(!store.verify(&secret(&first)));
    assert!(store.cached_is_empty(), "malformed change clears the cache");
    assert_eq!(store.reloads(), 1, "failed reads do not commit a stamp");
}

#[test]
fn generation_unstable_through_all_retries_denies_cached_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let minted = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let bearer = secret(&minted);
    let store = ReloadingTokenStore::load(path.clone()).unwrap();

    fs::write(&path, "{changed").unwrap();
    let changing_path = path;
    let accepted = store.with_current_observed(
        move |attempt| {
            let replacement = if attempt % 2 == 0 {
                "{changed-again"
            } else {
                "{changed"
            };
            fs::write(&changing_path, replacement).unwrap();
        },
        |snapshot| snapshot.verify(&bearer),
    );

    assert!(!accepted, "unstable changed state cannot retain old access");
    assert!(store.cached_is_empty(), "unstable change clears the cache");
    assert_eq!(store.reloads(), 0);
}

#[test]
fn unchanged_store_is_statted_without_re_reading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let minted = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let bearer = secret(&minted);
    let store = ReloadingTokenStore::load(path).unwrap();
    for _ in 0..8 {
        assert!(store.verify(&bearer));
    }
    assert_eq!(store.reloads(), 0);
}

#[test]
fn missing_generation_is_cached_while_creation_and_deletion_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let store = ReloadingTokenStore::load(path.clone()).unwrap();
    let absent_bearer = [0x55; TOKEN_LEN];

    for _ in 0..8 {
        assert!(!store.verify(&absent_bearer));
    }
    assert_eq!(
        store.reloads(),
        0,
        "an unchanged missing generation is not re-read"
    );

    let minted = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let bearer = secret(&minted);
    assert!(store.verify(&bearer), "later store creation is discovered");
    assert_eq!(store.reloads(), 1);

    fs::remove_file(&path).unwrap();
    assert!(!store.verify(&bearer), "store deletion revokes credentials");
    assert_eq!(store.reloads(), 2);
    assert!(!store.verify(&bearer));
    assert_eq!(
        store.reloads(),
        2,
        "the missing generation after deletion is cached too"
    );
}

#[test]
fn stable_reads_reject_credentials_deleted_during_initialization_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let stale = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let stale_bearer = secret(&stale);

    // Deletion lands between the first read and the generation probe.
    let init_path = path.clone();
    let store = ReloadingTokenStore::load_observed(path.clone(), move |attempt| {
        if attempt == 0 {
            fs::remove_file(&init_path).unwrap();
        }
    })
    .unwrap();
    assert!(store.is_empty());
    assert!(!store.verify(&stale_bearer));
    assert_eq!(store.reloads(), 0);

    for _ in 0..8 {
        assert!(!store.verify(&stale_bearer));
    }
    assert_eq!(store.reloads(), 0, "the stable absent result is cached");

    let current = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let current_bearer = secret(&current);
    assert!(
        store.verify(&current_bearer),
        "later creation is discovered"
    );
    assert_eq!(store.reloads(), 1);

    // Delete during a reload: the absent generation must be committed.
    mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let reload_path = path;
    let accepted = store.with_current_observed(
        move |attempt| {
            if attempt == 0 {
                fs::remove_file(&reload_path).unwrap();
            }
        },
        |snapshot| snapshot.verify(&current_bearer),
    );
    assert!(!accepted, "reload cannot cache a deleted credential");
    assert_eq!(store.reloads(), 2);
    assert!(!store.verify(&current_bearer));
    assert_eq!(
        store.reloads(),
        2,
        "the absent generation produced by the retry remains cached"
    );
}

#[test]
fn deleting_store_revokes_all_on_next_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let minted = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let bearer = secret(&minted);
    let store = ReloadingTokenStore::load(path.clone()).unwrap();
    assert!(store.verify(&bearer));
    fs::remove_file(path).unwrap();
    assert!(!store.verify(&bearer));
    assert!(store.is_empty());
}

#[test]
fn revocation_affects_new_authentication_not_established_attestation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let minted = mint_credential(
        &path,
        Some("device:a"),
        &[TERMINAL_CONTROL_SCOPE.to_owned()],
        None,
    )
    .unwrap();
    let bearer = secret(&minted);
    let live = ReloadingTokenStore::load(path.clone()).unwrap();
    let established = live.authenticate(&bearer).unwrap();
    revoke_credential(&path, &minted.id).unwrap();
    assert!(
        live.authenticate(&bearer).is_none(),
        "next handshake is revoked"
    );
    assert_eq!(
        established.principal, "device:a",
        "established session retains its captured attestation"
    );
}
