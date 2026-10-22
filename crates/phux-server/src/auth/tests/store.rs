use super::*;

/// The watcher's view of a credential generation: active until revoked,
/// removed, or expired; an unparseable store stands for nothing.
#[test]
fn standing_follows_the_stores_current_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    write_test_credential(&path, &[0x55; TOKEN_LEN]);
    let store = ReloadingTokenStore::load(path.clone()).unwrap();
    assert_eq!(
        store.standing("test-credential", 1),
        Some(Standing::Active { expires_at: None })
    );
    assert_eq!(
        store.standing("test-credential", 2),
        Some(Standing::Revoked)
    );
    assert_eq!(store.standing("someone-else", 1), Some(Standing::Revoked));

    rotate_credential(&path, "test-credential", Duration::zero()).unwrap();
    assert_eq!(
        store.standing("test-credential", 1),
        Some(Standing::Expired),
        "a rotated-out generation expires with its overlap"
    );
    assert!(matches!(
        store.standing("test-credential", 2),
        Some(Standing::Active { .. })
    ));

    revoke_credential(&path, "test-credential").unwrap();
    assert_eq!(
        store.standing("test-credential", 2),
        Some(Standing::Revoked)
    );

    write_test_credential(&path, &[0x55; TOKEN_LEN]);
    assert!(matches!(
        store.standing("test-credential", 1),
        Some(Standing::Active { .. })
    ));
    // None is a verdict for admitted sessions; new ones still fail closed.
    let replace = |contents: &str| {
        let staged = dir.path().join("staged");
        std::fs::write(&staged, contents).unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::rename(&staged, &path).unwrap();
    };
    replace("{ not a store");
    assert_eq!(store.standing("test-credential", 1), None, "malformed");
    replace("");
    assert_eq!(store.standing("test-credential", 1), None, "empty");
    std::fs::write(&path, "{\"version\":1,\"credentials\":[{\"id\":").unwrap();
    assert_eq!(
        store.standing("test-credential", 1),
        None,
        "caught mid-write"
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(store.standing("test-credential", 1), None, "missing");
    write_test_credential(&path, &[0x55; TOKEN_LEN]);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(store.standing("test-credential", 1), None, "insecure");
    assert!(
        !store.verify(&[0x55; TOKEN_LEN]),
        "a new admission fails closed against an insecure store"
    );
}

#[test]
fn structured_mint_persists_only_a_redacted_verifier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let minted = mint_credential(
        &path,
        Some("device:cockpit"),
        &[TERMINAL_CONTROL_SCOPE.to_owned()],
        None,
    )
    .unwrap();
    let raw = fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"version\": 1"));
    assert!(raw.contains("device:cockpit"));
    assert!(raw.contains("sha256:"));
    assert!(!raw.contains(minted.secret()));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let auth = TokenStore::load(&path)
        .unwrap()
        .authenticate(&secret(&minted))
        .unwrap();
    assert_eq!(auth.id, minted.id);
    assert_eq!(auth.principal, "device:cockpit");
    assert_eq!(auth.scopes, [TERMINAL_CONTROL_SCOPE]);
    assert_eq!(auth.generation, 1);
}

/// A store of anonymous token lines (the format before versioning) is
/// refused by name, admits nobody, and is never rewritten (ADR-0179).
#[test]
fn legacy_store_is_refused_and_left_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let token = "ab".repeat(TOKEN_LEN);
    let legacy = format!("# old\n{token}\n");
    fs::write(&path, &legacy).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        TokenStore::load(&path),
        Err(AuthError::LegacyStore)
    ));
    let message = AuthError::LegacyStore.to_string();
    assert!(
        message.contains("remove it") && message.contains("phux pair"),
        "the refusal names the remedy: {message}"
    );
    let (store, error) = ReloadingTokenStore::load_deferred(path.clone());
    assert!(matches!(error, Some(AuthError::LegacyStore)));
    assert!(!store.verify(&hex::decode(&token).unwrap()));
    assert!(
        matches!(mint_token(&path), Err(AuthError::LegacyStore)),
        "minting refuses rather than replacing the legacy file"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), legacy);
}

/// A store that will not load at boot starts empty and is adopted once
/// it loads.
#[test]
fn deferred_store_admits_nobody_until_the_file_loads_then_adopts_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    fs::write(&path, "{\"version\":1").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let bearer = [0x6c; TOKEN_LEN];

    let (store, error) = ReloadingTokenStore::load_deferred(path.clone());
    assert!(
        matches!(error, Some(AuthError::Malformed(_))),
        "the boot-time error comes back so the caller can report it"
    );
    assert!(store.is_empty());
    assert!(!store.verify(&bearer), "a deferred store admits nobody");

    // The operator repairs the store; no restart follows.
    write_test_credential(&path, &bearer);
    assert!(
        store.verify(&bearer),
        "the store adopts the repaired file on its next read"
    );
    assert_eq!(store.reloads(), 1);
}

#[test]
fn expiry_and_revocation_fail_closed() {
    let now = Utc::now();
    let token = [0x44; TOKEN_LEN];
    let base = CredentialRecord {
        id: "cred".to_owned(),
        verifier: verifier(&token),
        principal: "device:test".to_owned(),
        scopes: vec![TERMINAL_CONTROL_SCOPE.to_owned()],
        issued_at: now - Duration::minutes(1),
        expires_at: Some(now + Duration::seconds(1)),
        revoked_at: None,
        generation: 1,
        last_seen: None,
    };
    let store = TokenStore {
        file: CredentialFile {
            version: 1,
            credentials: vec![base.clone()],
        },
    };
    assert!(store.authenticate_at(&token, now).is_some());
    assert!(
        store
            .authenticate_at(&token, now + Duration::seconds(1))
            .is_none()
    );
    let mut revoked = base;
    revoked.expires_at = None;
    revoked.revoked_at = Some(now);
    let store = TokenStore {
        file: CredentialFile {
            version: 1,
            credentials: vec![revoked],
        },
    };
    assert!(store.authenticate_at(&token, now).is_none());
}

#[test]
fn insecure_store_types_permissions_and_ownership_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let minted = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let bearer = secret(&minted);
    let live = ReloadingTokenStore::load(path.clone()).unwrap();
    assert!(live.verify(&bearer));

    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(matches!(
        TokenStore::load(&path),
        Err(AuthError::InsecureStore(_))
    ));
    assert!(!live.verify(&bearer), "reload fails closed on unsafe mode");
    assert!(live.cached_is_empty(), "integrity failure clears the cache");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(live.verify(&bearer), "repair restores authentication");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o200)).unwrap();
    assert!(matches!(
        TokenStore::load(&path),
        Err(AuthError::InsecureStore(_))
    ));
    assert!(!live.verify(&bearer), "owner-unreadable stores fail closed");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let metadata = fs::symlink_metadata(&path).unwrap();
    assert!(matches!(
        validate_store_metadata_for_uid(&path, &metadata, metadata.uid().saturating_add(1)),
        Err(AuthError::InsecureStore(_))
    ));

    let target = dir.path().join("target");
    fs::rename(&path, &target).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(matches!(
        TokenStore::load(&path),
        Err(AuthError::InsecureStore(_))
    ));
    assert!(!live.verify(&bearer), "reload fails closed on symlinks");

    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(matches!(
        TokenStore::load(&path),
        Err(AuthError::InsecureStore(_))
    ));
}

#[test]
fn unknown_store_and_record_fields_are_denied() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let minted = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["unknown"] = serde_json::json!(true);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        TokenStore::load(&path),
        Err(AuthError::Malformed(_))
    ));

    value.as_object_mut().unwrap().remove("unknown");
    value["credentials"][0]["unknown"] = serde_json::json!(minted.id);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        TokenStore::load(&path),
        Err(AuthError::Malformed(_))
    ));
}

#[test]
fn debug_output_never_contains_bearer_or_verifier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let minted = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let raw = fs::read_to_string(&path).unwrap();
    let verifier = raw
        .split("sha256:")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let output = format!(
        "{minted:?} {:?} {:?}",
        TokenStore::load(&path).unwrap(),
        ReloadingTokenStore::load(path).unwrap()
    );
    assert!(!output.contains(minted.secret()));
    assert!(!output.contains(verifier));
    assert!(output.contains("[REDACTED]"));
}

#[test]
fn list_summarizes_credentials_and_prune_revokes_unused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let fresh = mint_token(&path).unwrap();
    let stale = mint_token(&path).unwrap();

    // Age the second credential so prune --unused-for 1h selects it.
    let mut file: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for cred in file["credentials"].as_array_mut().unwrap() {
        if cred["id"] == stale.id {
            cred["issued_at"] = serde_json::json!("2000-01-01T00:00:00Z");
        }
    }
    fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    // Restore owner-only mode after the raw rewrite.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

    let listed = list_credentials(&path).unwrap();
    assert_eq!(listed.len(), 2);
    let stale_row = listed.iter().find(|row| row.id == stale.id).unwrap();
    assert!(stale_row.last_seen.is_none());
    assert!(!stale_row.revoked);

    let pruned = prune_unused(&path, Duration::hours(1)).unwrap();
    assert_eq!(pruned.revoked_ids, vec![stale.id.clone()]);
    assert!(
        list_credentials(&path)
            .unwrap()
            .iter()
            .find(|row| row.id == stale.id)
            .unwrap()
            .revoked
    );
    assert!(
        !list_credentials(&path)
            .unwrap()
            .iter()
            .find(|row| row.id == fresh.id)
            .unwrap()
            .revoked
    );
    assert!(TokenStore::load(&path).unwrap().verify(&secret(&fresh)));
    assert!(!TokenStore::load(&path).unwrap().verify(&secret(&stale)));
}

#[test]
fn mint_replacing_revokes_the_previous_secret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let first = mint_token(&path).unwrap();
    let second = mint_terminal_credential(&path, Some(first.secret()), None).unwrap();
    assert_ne!(first.id, second.id);
    assert_ne!(first.secret(), second.secret());
    let store = TokenStore::load(&path).unwrap();
    assert!(!store.verify(&secret(&first)));
    assert!(store.verify(&secret(&second)));
    let listed = list_credentials(&path).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(
        listed
            .iter()
            .find(|row| row.id == first.id)
            .unwrap()
            .revoked
    );
    assert!(
        !listed
            .iter()
            .find(|row| row.id == second.id)
            .unwrap()
            .revoked
    );
}

#[test]
fn authenticate_and_touch_records_last_seen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let minted = mint_token(&path).unwrap();
    let store = ReloadingTokenStore::load(path.clone()).unwrap();
    assert!(store.authenticate_and_touch(&secret(&minted)).is_some());
    let listed = list_credentials(&path).unwrap();
    let row = listed.iter().find(|row| row.id == minted.id).unwrap();
    assert!(row.last_seen.is_some());
}
