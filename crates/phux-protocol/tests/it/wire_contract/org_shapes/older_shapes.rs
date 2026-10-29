//! Older shapes stay byte-identical, malformed events stay opaque, and
//! keys have one text form.

use crate::common::{assert_round_trip, decode, encode};
use phux_protocol::ids::{
    ClientId, IdempotencyKey, KEY_TEXT_LEN, PrincipalId, ResourceId, ResourceKey, Revision,
    SessionId, SessionKey, WindowId,
};
use phux_protocol::wire::frame::{
    ActorRef, AgentEvent, Command, CommandResult, CommandValue, FrameKind, SessionLifetime,
};
use phux_protocol::wire::info::{
    ResourceInfo, SessionInfo, SessionSnapshot, TUI_LAYOUT_KEY_PREFIX, tui_layout_key,
    tui_layout_key_session,
};

// -----------------------------------------------------------------------------
// Older shapes stay byte-identical.
// -----------------------------------------------------------------------------

fn changed(actor: Option<ActorRef>) -> FrameKind {
    FrameKind::MetadataChanged {
        scope: phux_protocol::wire::frame::Scope::Global,
        key: "phux.example/v1".to_owned(),
        value: None,
        actor,
    }
}

/// R09: an actor without principals writes exactly its `0.9.0` bytes, and a
/// pre-principal encoding decodes with both absent; with principals the pair
/// trails the earlier fields, which an older decoder skips because the actor
/// ends its container.
#[test]
fn actor_principals_trail_and_old_shapes_are_unchanged() {
    let plain = ActorRef::new(ClientId::new(7)).with_client_name(Some("cli".to_owned()));
    let with = plain
        .clone()
        .with_principal(PrincipalId::new([0x91; 16]))
        .with_on_behalf_of(PrincipalId::new([0x92; 16]));
    assert_round_trip(&changed(Some(with.clone())));
    let only_obo = plain
        .clone()
        .with_on_behalf_of(PrincipalId::new([0x92; 16]));
    assert_round_trip(&changed(Some(only_obo)));

    let old = encode(&changed(Some(plain.clone())));
    let new = encode(&changed(Some(with.clone())));
    // The new encoding is the old one plus `1 || 16 || 1 || 16` trailing the
    // actor (and four more length bytes in the frame header, and the field
    // length), so stripping the trailing pair reproduces the old bytes.
    assert_eq!(new.len(), old.len() + 34);
    let decoded = decode(&old).unwrap();
    let FrameKind::MetadataChanged {
        actor: Some(actor), ..
    } = decoded
    else {
        panic!("actor survives");
    };
    assert_eq!(actor, plain);
    assert_eq!(actor.principal(), None);
    assert_eq!(actor.on_behalf_of(), None);
    assert_eq!(with.principal(), PrincipalId::new([0x91; 16]));
}

/// R05/R06: a snapshot whose sessions and resources report no organization
/// state encodes exactly as before; reported state rides the extension block
/// (fields 3 and 4) and round-trips, and `with_lifetime` keeps `keep_empty`
/// equal to `lifetime == Persistent`.
#[test]
fn snapshot_session_link_and_lifetime_ride_the_extension_block() {
    let base = || {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(2))
            .with_sessions(vec![
                SessionInfo::new(SessionId::new(1), "work").with_window_count(1),
            ])
            .with_resources(vec![ResourceInfo::new(
                ResourceId::local(2),
                WindowId::new(1),
                80,
                24,
            )])
    };
    let state = |snapshot| FrameKind::CommandResult {
        request_id: 1,
        result: CommandResult::OkWith(CommandValue::State(snapshot)),
    };
    let legacy = encode(&state(base()));

    let key = SessionKey::new([0x5E; 16]).unwrap();
    let mut organized = base();
    organized.sessions[0] = organized.sessions[0]
        .clone()
        .with_lifetime(Some(SessionLifetime::Ephemeral))
        .with_key(Some(key));
    organized.resources[0] = organized.resources[0]
        .clone()
        .with_session(Some(SessionId::new(1)));
    assert!(!organized.sessions[0].keep_empty);
    let persistent = organized.sessions[0]
        .clone()
        .with_lifetime(Some(SessionLifetime::Persistent));
    assert!(persistent.keep_empty);

    let frame = state(organized.clone());
    assert_round_trip(&frame);
    let bytes = encode(&frame);
    assert!(bytes.len() > legacy.len());
    assert_eq!(encode(&state(base())), legacy);
    let FrameKind::CommandResult {
        result: CommandResult::OkWith(CommandValue::State(decoded)),
        ..
    } = decode(&bytes).unwrap()
    else {
        panic!("state reply");
    };
    assert_eq!(decoded.resources[0].session, Some(SessionId::new(1)));
    assert_eq!(
        decoded.sessions[0].lifetime,
        Some(SessionLifetime::Ephemeral)
    );
    assert_eq!(decoded.sessions[0].key, Some(key));
    assert!(decoded.sessions[0].attrs.is_empty());

    // Descriptive attributes (ADR-0168) ride the same SESSION_STATE.
    let attrs = vec![
        (
            "project.id".to_owned(),
            "github.com/no-phux/phux".to_owned(),
        ),
        ("project.linked_worktree".to_owned(), "true".to_owned()),
    ];
    let mut attributed = organized.clone();
    attributed.sessions[0] = attributed.sessions[0].clone().with_attrs(attrs.clone());
    let frame = state(attributed);
    assert_round_trip(&frame);
    let FrameKind::CommandResult {
        result: CommandResult::OkWith(CommandValue::State(decoded)),
        ..
    } = decode(&encode(&frame)).unwrap()
    else {
        panic!("state reply");
    };
    assert_eq!(decoded.sessions[0].attrs, attrs);
    assert_eq!(decoded.sessions[0].key, Some(key));
    assert_eq!(decoded.sessions[0].recent_input, None);

    // The recent input target rides the same SESSION_STATE, alone too.
    let mut recent = base();
    recent.sessions[0] = recent.sessions[0]
        .clone()
        .with_recent_input(Some(ResourceId::local(2)));
    let frame = state(recent);
    assert_round_trip(&frame);
    let FrameKind::CommandResult {
        result: CommandResult::OkWith(CommandValue::State(decoded)),
        ..
    } = decode(&encode(&frame)).unwrap()
    else {
        panic!("state reply");
    };
    assert_eq!(decoded.sessions[0].recent_input, Some(ResourceId::local(2)));
    assert_eq!(decoded.sessions[0].lifetime, None);

    // keep_empty is still written as the legacy facet.
    let keep = base();
    let mut keep_sessions = keep.sessions.clone();
    keep_sessions[0] = keep_sessions[0]
        .clone()
        .with_lifetime(Some(SessionLifetime::Persistent));
    assert_round_trip(&state(keep.with_sessions(keep_sessions)));
}

/// An organization event whose body names an unknown enum value or misses a
/// required field stays opaque, like any unknown event, instead of failing the
/// frame.
#[test]
fn malformed_organization_events_are_opaque() {
    let frame = FrameKind::Event {
        terminal: None,
        event: AgentEvent::SessionLifetimeChanged {
            session: SessionKey::new([1; 16]).unwrap(),
            lifetime: SessionLifetime::Ephemeral,
            revision: Revision::new(1),
        },
        stamp: None,
    };
    let mut bytes = encode(&frame);
    // The lifetime byte (`1`) is the only `0x01` following the `06 04 01`
    // field header; set it to an undefined lifetime.
    let at = bytes
        .windows(3)
        .position(|w| w == [0x06, 0x04, 0x01])
        .unwrap();
    bytes[at + 3] = 0x09;
    let FrameKind::Event { event, .. } = decode(&bytes).unwrap() else {
        panic!("event");
    };
    assert!(
        matches!(event, AgentEvent::Unknown { tag: 0x13, .. }),
        "{event:?}"
    );
}

/// An older client still sends `expected` (field 4) on a remove, a delete,
/// and the label edits; a decoder accepts and drops it. A relaunch body is
/// the old remove body, field for field, so retagging it is that old frame.
#[test]
fn an_ignored_expected_revision_still_decodes() {
    let operation = IdempotencyKey::new([3; 16]).unwrap();
    let resource = ResourceKey::new([5; 16]).unwrap();
    let mut bytes = encode(&FrameKind::Command {
        request_id: 7,
        command: Command::ResourceRelaunch {
            operation,
            resource,
            expected: Revision::new(9),
        },
    });
    // The nested tag, then field id 1 (`operation`).
    let at = bytes.windows(2).position(|w| w == [0x29, 0x01]).unwrap();
    bytes[at] = 0x28;
    assert_eq!(
        decode(&bytes).unwrap(),
        FrameKind::Command {
            request_id: 7,
            command: Command::ResourceRemove {
                operation,
                resource,
            },
        }
    );
}

/// The 16-byte keys print as 26 lowercase base32 digits and parse back in
/// either case; a short prefix matches, and zero keys do not exist.
#[test]
fn keys_have_one_text_form() {
    let key = SessionKey::new([0xFF; 16]).unwrap();
    let text = key.to_string();
    assert_eq!(text.len(), KEY_TEXT_LEN);
    assert_eq!(text, "77777777777777777777777774");
    assert_eq!(SessionKey::parse(&text), Some(key));
    assert_eq!(SessionKey::parse(&text.to_uppercase()), Some(key));
    assert!(key.has_prefix("777777"));
    assert!(!key.has_prefix("abc"));
    let one = ResourceKey::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]).unwrap();
    assert_eq!(ResourceKey::parse(&one.to_string()), Some(one));
    assert_eq!(SessionKey::new([0; 16]), None);
    assert_eq!(SessionKey::parse(&"a".repeat(KEY_TEXT_LEN)), None);
    // The two padding bits of the last digit must be zero.
    assert_eq!(SessionKey::parse("77777777777777777777777777"), None);
    assert_eq!(SessionKey::parse("short"), None);
    let json = serde_json::to_string(&key).unwrap();
    assert_eq!(json, format!("\"{text}\""));
    assert_eq!(serde_json::from_str::<SessionKey>(&json).unwrap(), key);
}

#[test]
fn tui_layout_key_names_one_session() {
    assert_eq!(TUI_LAYOUT_KEY_PREFIX, "phux.tui.layout/v1/");
    assert_eq!(tui_layout_key(SessionId::new(7)), "phux.tui.layout/v1/7");
    assert_eq!(
        tui_layout_key_session("phux.tui.layout/v1/7"),
        Some(SessionId::new(7))
    );
    for other in [
        "phux.tui.layout/v1/",
        "phux.tui.layout/v1/07",
        "phux.tui.layout/v1/x",
        "phux.tui.layout/v12",
        "work.layout/v1/1",
    ] {
        assert_eq!(tui_layout_key_session(other), None, "{other}");
    }
}
