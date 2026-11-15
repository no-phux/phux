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

/// The per-client cap refuses the next distinct key without evicting or
/// growing anything; a held triple re-subscribes, another client is not
/// capped, and reaping a Terminal frees its slots.
#[test]
fn subscribe_enforces_the_per_client_cap() {
    let mut store = MetadataStore::default();
    let (hog, other) = (ClientId(1), ClientId(2));
    let terminal = WireResourceId::local(1);
    for n in 0..MAX_SUBSCRIPTIONS_PER_CLIENT {
        assert!(store.subscribe(hog, Scope::Resource(terminal.clone()), key(n)));
    }
    let held = |store: &MetadataStore| {
        store
            .subscriptions
            .iter()
            .filter(|(c, _, _)| *c == hog)
            .count()
    };
    assert!(
        !store.subscribe(hog, Scope::Global, key(0)),
        "the cap+1'th key"
    );
    assert_eq!(held(&store), MAX_SUBSCRIPTIONS_PER_CLIENT);
    let first = Scope::Resource(terminal.clone());
    assert_eq!(
        store.subscribers_for(&first, &key(0)),
        vec![hog],
        "refuse, never evict"
    );
    assert!(
        store.subscribe(hog, first, key(0)),
        "a held triple re-subscribes"
    );
    assert!(
        store.subscribe(other, Scope::Global, key(0)),
        "the cap is per client"
    );

    store.forget_resource(&terminal);
    assert!(
        store.subscribe(hog, Scope::Global, key(0)),
        "reaping frees the slots"
    );
}

/// `forget_resource` removes the Terminal's subscriptions only.
#[test]
fn forget_terminal_reaps_only_subscriptions_naming_that_terminal() {
    let mut store = MetadataStore::default();
    let client = ClientId(1);
    let dead = Scope::Resource(WireResourceId::local(1));
    let alive = Scope::Resource(WireResourceId::local(2));
    for scope in [&dead, &alive, &Scope::Global] {
        assert!(store.subscribe(client, scope.clone(), "k".to_owned()));
    }
    store.forget_resource(&WireResourceId::local(1));
    assert!(store.subscribers_for(&dead, "k").is_empty());
    assert_eq!(store.subscribers_for(&alive, "k"), vec![client]);
    assert_eq!(store.subscribers_for(&Scope::Global, "k"), vec![client]);
}
