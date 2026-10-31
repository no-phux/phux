use super::*;

fn key(n: usize) -> String {
    format!("phux.test.key/{n}/v1")
}

/// `metadata_broadcast` accepts only the intercepted keys.
#[test]
fn intercepted_key_variants_are_the_broadcast_only_session_writes() {
    assert_eq!(
        ServerInterceptedKey::SessionName.as_str(),
        phux_protocol::wire::frame::SESSION_NAME_KEY,
    );
    assert_eq!(
        ServerInterceptedKey::SessionKeepEmpty.as_str(),
        phux_protocol::wire::frame::SESSION_KEEP_EMPTY_KEY,
    );
}

/// Every intercepted key has a workload-auth catalog entry (ADR-0125)
/// its writes classify onto, and refused keys are denied.
#[test]
fn every_intercepted_key_has_a_catalog_classification() {
    use phux_protocol::kinds::{self, Carrier, Classification};
    use phux_protocol::wire::frame::{
        FrameKind, RESOURCE_PANE_OCCUPANT_KEY, SESSION_CREATE_KEY,
        SESSION_CREATE_RESULT_KEY_PREFIX, Scope, WHOAMI_KEY,
    };

    let set = |key: &str| FrameKind::SetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: key.to_owned(),
        value: b"x".to_vec(),
    };
    let intercepted = [
        ServerInterceptedKey::SessionName,
        ServerInterceptedKey::SessionKeepEmpty,
    ]
    .map(|key| match key {
        ServerInterceptedKey::SessionName | ServerInterceptedKey::SessionKeepEmpty => key.as_str(),
    });
    for key in intercepted.into_iter().chain([SESSION_CREATE_KEY]) {
        let method = kinds::method_named(key);
        assert!(method.is_some(), "{key} has no catalog entry");
        let method = method.unwrap_or_else(|| unreachable!());
        assert_eq!(method.carrier, Carrier::Metadata(key));
        let rule = kinds::frame_rule(&set(key));
        assert!(
            method.rules.iter().any(|row| std::ptr::eq(*row, rule)),
            "{key}: a Global SET lands on `{}`, which its entry does not name",
            rule.case
        );
    }
    let result = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}token");
    for key in [RESOURCE_PANE_OCCUPANT_KEY, WHOAMI_KEY, result.as_str()] {
        assert_eq!(
            kinds::classify_frame(&set(key)),
            Classification::Deny,
            "{key}"
        );
    }
}

/// The cap refuses the next distinct key without mutating the store.
#[test]
fn subscribe_enforces_the_per_client_cap() {
    let mut store = MetadataStore::default();
    let client = ClientId(1);

    for n in 0..MAX_SUBSCRIPTIONS_PER_CLIENT {
        assert!(
            store.subscribe(client, Scope::Global, key(n)),
            "subscription {n} is under the cap and must be accepted",
        );
    }
    assert_eq!(
        store
            .subscriptions
            .iter()
            .filter(|(c, _, _)| *c == client)
            .count(),
        MAX_SUBSCRIPTIONS_PER_CLIENT,
    );

    let refused = store.subscribe(client, Scope::Global, key(MAX_SUBSCRIPTIONS_PER_CLIENT));
    assert!(!refused, "the cap+1'th distinct key must be refused");
    assert_eq!(
        store
            .subscriptions
            .iter()
            .filter(|(c, _, _)| *c == client)
            .count(),
        MAX_SUBSCRIPTIONS_PER_CLIENT,
        "a refused subscribe must not grow the client's held count past the cap",
    );
    // Refuse, never evict a working subscription.
    assert_eq!(
        store.subscribers_for(&Scope::Global, &key(0)),
        vec![client],
        "a refused subscribe must not evict any existing subscription",
    );
}

/// Re-subscribing a held triple never trips the cap.
#[test]
fn resubscribing_an_existing_triple_at_the_cap_stays_accepted() {
    let mut store = MetadataStore::default();
    let client = ClientId(1);
    for n in 0..MAX_SUBSCRIPTIONS_PER_CLIENT {
        assert!(store.subscribe(client, Scope::Global, key(n)));
    }

    assert!(
        store.subscribe(client, Scope::Global, key(0)),
        "re-subscribing an existing triple must succeed even while at the cap",
    );
}

/// The cap is per client.
#[test]
fn the_cap_is_per_client_not_global() {
    let mut store = MetadataStore::default();
    let hog = ClientId(1);
    let other = ClientId(2);
    for n in 0..MAX_SUBSCRIPTIONS_PER_CLIENT {
        assert!(store.subscribe(hog, Scope::Global, key(n)));
    }
    assert!(!store.subscribe(hog, Scope::Global, key(MAX_SUBSCRIPTIONS_PER_CLIENT)));

    assert!(
        store.subscribe(other, Scope::Global, key(0)),
        "a different client must still be able to subscribe to the same key",
    );
}

/// `forget_resource` removes the Terminal's keys and subscriptions only.
#[test]
fn forget_terminal_reaps_only_subscriptions_naming_that_terminal() {
    let mut store = MetadataStore::default();
    let client = ClientId(1);
    let dead = WireResourceId::local(1);
    let alive = WireResourceId::local(2);

    assert!(store.subscribe(client, Scope::Resource(dead.clone()), "k".to_owned()));
    assert!(store.subscribe(client, Scope::Resource(alive.clone()), "k".to_owned()));
    assert!(store.subscribe(client, Scope::Global, "k".to_owned()));

    store.forget_resource(&dead);

    assert!(
        store
            .subscribers_for(&Scope::Resource(dead.clone()), "k")
            .is_empty(),
        "the dead Terminal's subscription must be reaped",
    );
    assert_eq!(
        store.subscribers_for(&Scope::Resource(alive), "k"),
        vec![client],
        "a different Terminal's subscription must survive",
    );
    assert_eq!(
        store.subscribers_for(&Scope::Global, "k"),
        vec![client],
        "a Global subscription must survive",
    );
}

/// Reaping a Terminal frees its subscriptions' cap slots.
#[test]
fn forget_terminal_frees_a_cap_slot_for_reuse() {
    let mut store = MetadataStore::default();
    let client = ClientId(1);
    let terminal = WireResourceId::local(1);

    for n in 0..MAX_SUBSCRIPTIONS_PER_CLIENT {
        assert!(store.subscribe(client, Scope::Resource(terminal.clone()), key(n)));
    }
    assert!(!store.subscribe(client, Scope::Global, "overflow".to_owned()));

    store.forget_resource(&terminal);

    assert!(
        store.subscribe(client, Scope::Global, "overflow".to_owned()),
        "reaping the dead Terminal's subscriptions must free room under the cap",
    );
}
