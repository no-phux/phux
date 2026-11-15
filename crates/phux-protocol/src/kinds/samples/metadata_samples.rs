// The metadata frames, whose scope, key and value pick the row.

fn set(scope: Scope, key: &str, value: &[u8]) -> FrameKind {
    FrameKind::SetMetadata {
        request_id: 1,
        scope,
        key: key.to_owned(),
        value: value.to_vec(),
    }
}

fn delete(key: &str) -> FrameKind {
    FrameKind::DeleteMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: key.to_owned(),
    }
}

fn subscribe(key: &str) -> FrameKind {
    FrameKind::SubscribeMetadata {
        scope: Scope::Global,
        key: key.to_owned(),
    }
}

fn metadata_samples() -> Vec<(FrameKind, &'static Rule)> {
    let result = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}token");
    let pane = Scope::Resource(terminal());
    let mark = encode_session_keep_empty("work", true);
    let unmark = encode_session_keep_empty("work", false);
    vec![
        (
            FrameKind::GetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: WHOAMI_KEY.to_owned(),
            },
            &F_WHOAMI,
        ),
        (
            FrameKind::GetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: RESOURCE_AGENT_KEY.to_owned(),
            },
            &F_GET_METADATA,
        ),
        (
            set(Scope::Global, SESSION_CREATE_KEY, b"{}"),
            &F_SESSION_CREATE,
        ),
        (
            set(Scope::Global, SESSION_NAME_KEY, b"old\0new"),
            &F_METADATA_WRITE,
        ),
        (
            set(Scope::Global, SESSION_KEEP_EMPTY_KEY, &mark),
            &F_KEEP_EMPTY_MARK,
        ),
        (
            set(Scope::Global, SESSION_KEEP_EMPTY_KEY, &unmark),
            &F_KEEP_EMPTY_CLEAR,
        ),
        (
            set(Scope::Global, SESSION_KEEP_EMPTY_KEY, b"work"),
            &F_KEEP_EMPTY_OTHER,
        ),
        (
            set(Scope::Global, CONFIG_RELOAD_KEY, b"1"),
            &F_CONFIG_RELOAD,
        ),
        (
            set(Scope::Global, SESSION_CREATE_RESULT_KEY, b"x"),
            &F_RESULT_NAMESPACE_WRITE,
        ),
        (set(Scope::Global, &result, b"x"), &F_RESULT_NAMESPACE_WRITE),
        (
            set(pane.clone(), RESOURCE_PANE_OCCUPANT_KEY, b"x"),
            &F_SERVER_OWNED_WRITE,
        ),
        (set(Scope::Global, WHOAMI_KEY, b"x"), &F_SERVER_OWNED_WRITE),
        (
            set(Scope::Global, SESSION_KEEP_EMPTY_KEY, b"work\0yes"),
            &F_KEEP_EMPTY_OTHER,
        ),
        // The special rows are Global-scoped; elsewhere the key is ordinary.
        (
            set(pane.clone(), SESSION_CREATE_KEY, b"{}"),
            &F_METADATA_WRITE,
        ),
        (set(pane, RESOURCE_TAGS_KEY, b"[]"), &F_METADATA_WRITE),
        (delete(SESSION_CREATE_RESULT_KEY), &F_RESULT_NAMESPACE_WRITE),
        (delete(RESOURCE_PANE_OCCUPANT_KEY), &F_SERVER_OWNED_WRITE),
        (delete(WHOAMI_KEY), &F_SERVER_OWNED_WRITE),
        (delete(CONFIG_RELOAD_KEY), &F_SERVER_OWNED_WRITE),
        (delete(SESSION_KEEP_EMPTY_KEY), &F_SERVER_OWNED_WRITE),
        (delete(SESSION_CREATE_KEY), &F_METADATA_WRITE),
        (delete(&result), &F_RESULT_NAMESPACE_WRITE),
        (subscribe(SESSION_CREATE_RESULT_KEY), &F_RESULT_NAMESPACE_SUBSCRIBE),
        (
            FrameKind::ListMetadata {
                request_id: 1,
                scope: Scope::Global,
            },
            &F_LIST_METADATA,
        ),
        (subscribe(&result), &F_RESULT_NAMESPACE_SUBSCRIBE),
        (subscribe(RESOURCE_AGENT_KEY), &F_SUBSCRIBE_METADATA),
    ]
    .into_iter()
    .chain(approval_samples())
    .chain(server_owned_samples())
    .collect()
}

/// The server-owned keys outside the approval records, in any scope.
fn server_owned_samples() -> Vec<(FrameKind, &'static Rule)> {
    use crate::wire::frame::{
        RESOURCE_CONTROLLER_KEY, RESOURCE_PROGRAM_STATUS_KEY, RESOURCE_PROGRAM_STATUS_RECORD_PREFIX,
    };
    let mut samples = Vec::new();
    for key in [
        RESOURCE_CONTROLLER_KEY,
        RESOURCE_PROGRAM_STATUS_KEY,
        RESOURCE_PROGRAM_STATUS_RECORD_PREFIX,
        "phux.program-status.record/v1/build/test",
    ] {
        samples.push((set(Scope::Resource(terminal()), key, b"x"), &F_SERVER_OWNED_WRITE));
        samples.push((delete(key), &F_SERVER_OWNED_WRITE));
    }
    samples
}

/// The decision key and the approval records (ADR-0128).
fn approval_samples() -> Vec<(FrameKind, &'static Rule)> {
    let id = "0123456789abcdef0123456789abcdef";
    let decide = format!("{APPROVAL_DECIDE_KEY_PREFIX}{id}");
    let record = format!("{APPROVAL_KEY_PREFIX}{id}");
    let pane = Scope::Resource(terminal());
    vec![
        (set(Scope::Global, &decide, b"approve"), &F_APPROVAL_DECIDE),
        (set(Scope::Global, &decide, b"deny"), &F_APPROVAL_DECIDE),
        (
            set(Scope::Global, &decide, b"maybe"),
            &F_APPROVAL_DECIDE_OTHER,
        ),
        (
            set(
                Scope::Global,
                &format!("{APPROVAL_DECIDE_KEY_PREFIX}NOT-AN-ID"),
                b"approve",
            ),
            &F_APPROVAL_DECIDE_OTHER,
        ),
        (set(Scope::Global, &record, b"{}"), &F_SERVER_OWNED_WRITE),
        (set(pane.clone(), &record, b"{}"), &F_SERVER_OWNED_WRITE),
        (delete(&record), &F_SERVER_OWNED_WRITE),
        (delete(&decide), &F_SERVER_OWNED_WRITE),
        // Only the Global decision is intercepted; elsewhere it is ordinary.
        (set(pane, &decide, b"approve"), &F_METADATA_WRITE),
    ]
}
