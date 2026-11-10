//! Satellite panes on a hub: only the keys the hub federates (ADR-0136).
use super::*;

#[test]
fn a_satellite_pane_is_not_subscribed_to_the_occupant_record_a_hub_cannot_route() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, None);
    let satellite = ResourceId::satellite("powerhouse-1", 3);
    let mut current = snapshot();
    current.resources.push(ResourceInfo::new(
        satellite.clone(),
        WindowId::new(1),
        20,
        4,
    ));
    let frames = refresh_after_settling(&mut plane, current);
    let keys: Vec<&str> = frames
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::SubscribeMetadata {
                scope: Scope::Resource(id),
                key,
            } if *id == satellite => Some(key.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(keys, [RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY], "{frames:?}");
}
