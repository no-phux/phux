use super::*;

fn register_device(
    fx: &Fixture,
    key: &[u8],
    scope: &str,
    principal: Option<EnrolledFor>,
) -> String {
    WorkloadRegistry::register_replacing(
        &fx.paths.registry,
        key,
        scopes(&[scope]),
        Some(in_an_hour()),
        None,
        principal,
    )
    .unwrap()
    .id
}

#[test]
fn repair_updates_only_the_named_legacy_device_and_is_idempotent() {
    let fx = fixture();
    let phone = register_device(
        &fx,
        b"phone",
        LEGACY_PAIRED_DEVICE_SCOPE,
        Some(EnrolledFor::Human),
    );
    let bot = register_device(&fx, b"bot", "observe@host", Some(EnrolledFor::Automation));
    let before = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    let old = before.lookup(&phone).unwrap();
    let repaired = WorkloadRegistry::repair_paired_device(&fx.paths.registry, &phone).unwrap();
    assert!(repaired.updated);
    assert_eq!(repaired.generation, before.generation() + 1);
    let after = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    let mut expected = old.clone();
    expected.scopes = scopes(&[PAIRED_DEVICE_SCOPE]);
    assert_eq!(
        after.lookup(&phone),
        Some(&expected),
        "key, expiry and principal preserved"
    );
    assert_eq!(before.lookup(&bot), after.lookup(&bot));
    assert_eq!(before.instance_id(), after.instance_id());
    let bytes = std::fs::read(&fx.paths.registry).unwrap();
    let repeated = WorkloadRegistry::repair_paired_device(&fx.paths.registry, &phone).unwrap();
    assert!(!repeated.updated);
    assert_eq!(repeated.generation, repaired.generation);
    assert_eq!(
        std::fs::read(&fx.paths.registry).unwrap(),
        bytes,
        "repeat writes nothing"
    );
}

#[test]
fn repair_keeps_an_inferred_legacy_device_human() {
    let fx = fixture();
    let phone = register_device(&fx, b"phone", LEGACY_PAIRED_DEVICE_SCOPE, None);
    WorkloadRegistry::repair_paired_device(&fx.paths.registry, &phone).unwrap();
    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    let credential = registry.lookup(&phone).unwrap();
    assert_eq!(credential.recorded_for, Some(EnrolledFor::Human));
    assert_eq!(credential.enrolled_for(), EnrolledFor::Human);
}

#[test]
fn repair_never_widens_custom_or_automation_scopes() {
    let fx = fixture();
    for (index, scope, principal) in [
        (0, "observe,input@global", Some(EnrolledFor::Human)),
        (
            1,
            "inventory,observe,create,bind,input,signal@host",
            Some(EnrolledFor::Human),
        ),
        (
            2,
            "inventory,observe,create,bind,input,?signal@global",
            Some(EnrolledFor::Human),
        ),
        (3, LEGACY_PAIRED_DEVICE_SCOPE, Some(EnrolledFor::Automation)),
        (4, "*@global", None),
        (5, PAIRED_DEVICE_SCOPE, Some(EnrolledFor::Automation)),
    ] {
        let id = register_device(&fx, &[index + 1], scope, principal);
        let bytes = std::fs::read(&fx.paths.registry).unwrap();
        assert!(
            WorkloadRegistry::repair_paired_device(&fx.paths.registry, &id).is_err(),
            "{scope} {principal:?}"
        );
        assert_eq!(std::fs::read(&fx.paths.registry).unwrap(), bytes);
    }
}

#[test]
fn repair_refuses_inactive_satellite_and_invalid_credentials_without_writing() {
    let fx = fixture();
    let revoked = register_device(
        &fx,
        b"revoked",
        LEGACY_PAIRED_DEVICE_SCOPE,
        Some(EnrolledFor::Human),
    );
    WorkloadRegistry::revoke(&fx.paths.registry, &revoked).unwrap();
    let expired = register_device(
        &fx,
        b"expired",
        LEGACY_PAIRED_DEVICE_SCOPE,
        Some(EnrolledFor::Human),
    );
    let satellite = WorkloadRegistry::register_satellite(
        &fx.paths.registry,
        b"satellite",
        "edge",
        in_an_hour(),
    )
    .unwrap()
    .id;
    // Expired records remain valid registry history; enrollment itself refuses
    // an already-past expiry, so age just this disposable fixture afterwards.
    let mut image: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fx.paths.registry).unwrap()).unwrap();
    image["credentials"][1]["expires_at"] = serde_json::json!(1);
    std::fs::write(&fx.paths.registry, serde_json::to_vec(&image).unwrap()).unwrap();
    let bytes = std::fs::read(&fx.paths.registry).unwrap();
    for id in [
        &revoked,
        &expired,
        &satellite,
        &credential_id(b"absent"),
        "not-an-id",
    ] {
        assert!(
            WorkloadRegistry::repair_paired_device(&fx.paths.registry, id).is_err(),
            "{id}"
        );
        assert_eq!(std::fs::read(&fx.paths.registry).unwrap(), bytes);
    }
}

#[test]
fn principal_inference_stays_bound_to_the_legacy_pairing_origin() {
    assert_eq!(
        EnrolledFor::by_ceiling(&scopes(&[LEGACY_PAIRED_DEVICE_SCOPE])),
        EnrolledFor::Human
    );
    assert_eq!(
        EnrolledFor::by_ceiling(&scopes(&["*@global"])),
        EnrolledFor::Automation
    );
    assert_eq!(
        EnrolledFor::by_ceiling(&scopes(&[PAIRED_DEVICE_SCOPE])),
        EnrolledFor::Automation
    );
}
