use super::*;

#[test]
fn grant_and_ungrant_change_one_scope_and_keep_the_rest() {
    let fx = fixture();
    let path = &fx.paths.registry;
    let phone = WorkloadRegistry::register_replacing(
        path,
        b"phone",
        scopes(&[PAIRED_DEVICE_SCOPE]),
        Some(in_an_hour()),
        None,
        None,
    )
    .unwrap()
    .id;
    let before = WorkloadRegistry::load(path).unwrap();
    let old = before.lookup(&phone).unwrap().clone();

    let granted = WorkloadRegistry::change_scope(path, &phone, "input@display", true).unwrap();
    assert!(granted.updated);
    assert_eq!(granted.generation, before.generation() + 1);
    assert_eq!(
        granted.scopes,
        scopes(&[PAIRED_DEVICE_SCOPE, "input@display"])
    );
    let after = WorkloadRegistry::load(path).unwrap();
    let record = after.lookup(&phone).unwrap();
    assert_eq!(
        (&record.public_key, record.expires_at, record.enrolled_for()),
        (&old.public_key, old.expires_at, old.enrolled_for()),
        "key, expiry and principal preserved"
    );

    // A repeat changes nothing; a spelling of the same grant is the same.
    let repeat = WorkloadRegistry::change_scope(path, &phone, "input@display", true).unwrap();
    assert!(!repeat.updated);
    assert_eq!(repeat.generation, granted.generation);

    let removed = WorkloadRegistry::change_scope(path, &phone, "input@display", false).unwrap();
    assert!(removed.updated);
    assert_eq!(removed.scopes, scopes(&[PAIRED_DEVICE_SCOPE]));
    assert!(
        !WorkloadRegistry::change_scope(path, &phone, "input@display", false)
            .unwrap()
            .updated
    );
}

#[test]
fn grant_refuses_bad_scopes_satellites_and_the_last_scope() {
    let fx = fixture();
    let path = &fx.paths.registry;
    let bot = WorkloadRegistry::register_replacing(
        path,
        b"bot",
        scopes(&["observe@host"]),
        Some(in_an_hour()),
        None,
        None,
    )
    .unwrap()
    .id;
    let bytes = std::fs::read(path).unwrap();
    assert!(matches!(
        WorkloadRegistry::change_scope(path, &bot, "input@screen", true),
        Err(WorkloadError::InvalidScope { .. })
    ));
    assert!(matches!(
        WorkloadRegistry::change_scope(path, &bot, "input@terminal:3", true),
        Err(WorkloadError::UnstableSelector { .. })
    ));
    assert!(matches!(
        WorkloadRegistry::change_scope(path, &bot, "observe@host", false),
        Err(WorkloadError::Malformed(_))
    ));
    assert!(matches!(
        WorkloadRegistry::change_scope(path, "sha256:nope", "input@page", true),
        Err(WorkloadError::InvalidCredentialId)
    ));
    WorkloadRegistry::revoke(path, &bot).unwrap();
    let revoked = std::fs::read(path).unwrap();
    assert_ne!(bytes, revoked);
    assert!(matches!(
        WorkloadRegistry::change_scope(path, &bot, "input@page", true),
        Err(WorkloadError::Malformed(_))
    ));
    assert_eq!(
        std::fs::read(path).unwrap(),
        revoked,
        "a refusal writes nothing"
    );
}
