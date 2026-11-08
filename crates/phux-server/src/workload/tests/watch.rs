//! The watch grants a person's paired device holds (`docs/authority.md`):
//! added at pairing, and once to devices paired before they existed.

use super::*;

fn register(fx: &Fixture, key: &[u8], scope: &str, principal: Option<EnrolledFor>) -> String {
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

fn scopes_of(fx: &Fixture, id: &str) -> Vec<String> {
    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    registry.lookup(id).unwrap().scopes.clone()
}

fn watched(scope: &str) -> Vec<String> {
    scopes(&[scope, "observe@display", "observe@window"])
}

#[test]
fn pairing_a_person_adds_the_watch_grants_and_an_automation_key_gets_none() {
    let fx = fixture();
    let phone = register(&fx, b"phone", PAIRED_DEVICE_SCOPE, Some(EnrolledFor::Human));
    let ci = register(
        &fx,
        b"ci",
        PAIRED_DEVICE_SCOPE,
        Some(EnrolledFor::Automation),
    );
    let narrow = register(&fx, b"narrow", "observe@host", Some(EnrolledFor::Human));
    assert_eq!(scopes_of(&fx, &phone), watched(PAIRED_DEVICE_SCOPE));
    assert_eq!(scopes_of(&fx, &ci), scopes(&[PAIRED_DEVICE_SCOPE]));
    assert_eq!(scopes_of(&fx, &narrow), scopes(&["observe@host"]));
}

#[test]
fn the_migration_gives_old_paired_people_the_watch_grants_once() {
    let fx = fixture();
    let path = &fx.paths.registry;
    let ci = register(
        &fx,
        b"ci",
        PAIRED_DEVICE_SCOPE,
        Some(EnrolledFor::Automation),
    );
    // Written as a build before watch grants recorded a paired phone.
    let phone = register(&fx, b"phone", PAIRED_DEVICE_SCOPE, None);
    let legacy = register(&fx, b"legacy", LEGACY_PAIRED_DEVICE_SCOPE, None);
    let wildcard = register(&fx, b"wildcard", "*@global", None);
    let mut image: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    image["credentials"][1]["principal"] = serde_json::json!("human");
    std::fs::write(path, serde_json::to_vec(&image).unwrap()).unwrap();

    let changed = WorkloadRegistry::migrate_paired_watch(path).unwrap();
    assert_eq!(changed, Some(vec![phone.clone(), legacy.clone()]));
    assert_eq!(scopes_of(&fx, &phone), watched(PAIRED_DEVICE_SCOPE));
    assert_eq!(scopes_of(&fx, &legacy), watched(LEGACY_PAIRED_DEVICE_SCOPE));
    let registry = WorkloadRegistry::load(path).unwrap();
    assert_eq!(
        registry.lookup(&legacy).unwrap().enrolled_for(),
        EnrolledFor::Human
    );
    assert_eq!(scopes_of(&fx, &ci), scopes(&[PAIRED_DEVICE_SCOPE]));
    assert_eq!(scopes_of(&fx, &wildcard), scopes(&["*@global"]));

    // Once: an ungrant afterwards is never undone.
    for scope in ["observe@display", "observe@window"] {
        WorkloadRegistry::change_scope(path, &phone, scope, false).unwrap();
    }
    assert_eq!(WorkloadRegistry::migrate_paired_watch(path).unwrap(), None);
    assert_eq!(scopes_of(&fx, &phone), scopes(&[PAIRED_DEVICE_SCOPE]));

    // Repairing a migrated legacy device keeps what it was given.
    assert!(
        WorkloadRegistry::repair_paired_device(path, &legacy)
            .unwrap()
            .updated
    );
    assert_eq!(scopes_of(&fx, &legacy), watched(PAIRED_DEVICE_SCOPE));
}

#[test]
fn no_registry_means_nothing_to_migrate_and_no_marker() {
    let fx = fixture();
    assert_eq!(
        WorkloadRegistry::migrate_paired_watch(&fx.paths.registry).unwrap(),
        None
    );
    let phone = register(&fx, b"phone", LEGACY_PAIRED_DEVICE_SCOPE, None);
    assert_eq!(
        WorkloadRegistry::migrate_paired_watch(&fx.paths.registry).unwrap(),
        Some(vec![phone])
    );
}
