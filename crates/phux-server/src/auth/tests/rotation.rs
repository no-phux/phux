use super::*;

#[test]
fn rotation_has_bounded_ab_overlap_and_preserves_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let first = mint_credential(
        &path,
        Some("device:a"),
        &[TERMINAL_CONTROL_SCOPE.to_owned()],
        None,
    )
    .unwrap();
    let first_secret = secret(&first);
    let now = Timestamp::now() + SignedDuration::from_secs(1);
    let second = with_lock(&path, |held| {
        rotate_credential_at(&path, &first.id, SignedDuration::from_mins(5), now, held)
    })
    .unwrap();
    let second_secret = secret(&second);
    let store = TokenStore::load(&path).unwrap();
    assert_eq!(
        store
            .authenticate_at(&first_secret, now)
            .unwrap()
            .generation,
        1
    );
    let current = store.authenticate_at(&second_secret, now).unwrap();
    assert_eq!(current.id, first.id);
    assert_eq!(current.principal, "device:a");
    assert_eq!(current.generation, 2);
    assert!(
        store
            .authenticate_at(&first_secret, now + SignedDuration::from_mins(5))
            .is_none()
    );
    assert!(
        store
            .authenticate_at(&second_secret, now + SignedDuration::from_mins(5))
            .is_some()
    );
}

#[test]
fn rotation_preserves_absolute_expiry_for_new_and_overlap_generations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let now = Timestamp::now() + SignedDuration::from_secs(1);
    let expiry = now + SignedDuration::from_mins(2);
    let first = mint_credential(
        &path,
        Some("device:expiring"),
        &[TERMINAL_CONTROL_SCOPE.to_owned()],
        Some(expiry),
    )
    .unwrap();
    let first_secret = secret(&first);
    let second = with_lock(&path, |held| {
        rotate_credential_at(&path, &first.id, SignedDuration::from_mins(5), now, held)
    })
    .unwrap();
    let second_secret = secret(&second);
    let store = TokenStore::load(&path).unwrap();

    assert!(store.authenticate_at(&first_secret, now).is_some());
    let current = store.authenticate_at(&second_secret, now).unwrap();
    assert_eq!(current.expires_at, Some(expiry));
    assert!(store.authenticate_at(&first_secret, expiry).is_none());
    assert!(store.authenticate_at(&second_secret, expiry).is_none());
}

#[test]
fn rotation_rejects_an_already_expired_credential_without_changing_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let now = Timestamp::now() + SignedDuration::from_mins(1);
    let first = mint_credential(
        &path,
        Some("device:expired"),
        &[TERMINAL_CONTROL_SCOPE.to_owned()],
        Some(now - SignedDuration::from_secs(1)),
    )
    .unwrap();
    let before = fs::read(&path).unwrap();

    let result = with_lock(&path, |held| {
        rotate_credential_at(&path, &first.id, SignedDuration::from_mins(5), now, held)
    });

    assert!(matches!(result, Err(AuthError::CredentialExpired(id)) if id == first.id));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(TokenStore::load(&path).unwrap().len(), 1);
}

#[test]
fn fault_injected_rotation_is_old_or_new_at_each_atomic_commit_stage() {
    for stage in [
        WriteFault::BeforeTempSync,
        WriteFault::AfterTempSyncBeforeRename,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let first =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let _armed = fault::inject(stage);
        let result = with_lock(&path, |held| {
            rotate_credential_at(
                &path,
                &first.id,
                SignedDuration::from_mins(5),
                Timestamp::now(),
                held,
            )
        });
        assert!(result.is_err());
        let store = TokenStore::load(&path).unwrap();
        assert!(store.verify(&secret(&first)));
        assert_eq!(
            store.len(),
            1,
            "pre-rename interruption preserves old store"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let first = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let _armed = fault::inject(WriteFault::AfterRenameBeforeDirectorySync);
    let rotated = with_lock(&path, |held| {
        rotate_credential_at(
            &path,
            &first.id,
            SignedDuration::from_mins(5),
            Timestamp::now(),
            held,
        )
    })
    .unwrap();
    assert!(!rotated.is_durable(), "post-rename ambiguity is explicit");
    let store = TokenStore::load(&path).unwrap();
    assert!(
        store.verify(&secret(&rotated)),
        "renamed generation is visible"
    );
    assert_eq!(store.len(), 2);
}
