use super::note_agent;
use phux_protocol::ResourceId;
use std::collections::HashMap;

#[test]
fn a_repeated_badge_changes_nothing() {
    let terminal = ResourceId::local(1);
    let working = br#"{"name":"Claude","kind":"claude","state":"working"}"#;
    let blocked = br#"{"name":"Claude","kind":"claude","state":"blocked"}"#;
    let mut agents = HashMap::new();
    assert!(note_agent(&mut agents, &terminal, Some(working), 10));
    assert!(!note_agent(&mut agents, &terminal, Some(working), 20));
    assert_eq!(agents[&terminal].changed_at_ms, 10);
    assert!(note_agent(&mut agents, &terminal, Some(blocked), 30));
    assert_eq!(agents[&terminal].changed_at_ms, 30);
    assert!(note_agent(&mut agents, &terminal, None, 40));
    assert!(!note_agent(&mut agents, &terminal, None, 50));
    assert!(agents.is_empty());
}
