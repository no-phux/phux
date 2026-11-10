//! The C record layouts and the refusals a malformed header gets.
use super::*;

#[allow(
    clippy::cognitive_complexity,
    reason = "one walk over every layout pin"
)]
#[test]
fn abi_null_version_size_and_disconnect() {
    assert_eq!(std::mem::size_of::<PhuxArrangementInfo>(), 128);
    assert_eq!(std::mem::offset_of!(PhuxArrangementInfo, revision), 16);
    assert_eq!(std::mem::offset_of!(PhuxArrangementInfo, message), 56);
    assert_eq!(std::mem::offset_of!(PhuxArrangementInfo, backend), 72);
    assert_eq!(
        std::mem::offset_of!(PhuxArrangementInfo, server_revision),
        80
    );
    assert_eq!(std::mem::offset_of!(PhuxArrangementInfo, session_key), 88);
    assert_eq!(
        std::mem::offset_of!(PhuxArrangementInfo, arrangement_key),
        104
    );
    assert_eq!(
        std::mem::offset_of!(PhuxArrangementInfo, unarranged_count),
        120
    );
    assert_eq!(
        std::mem::offset_of!(PhuxArrangementInfo, unplaced_count),
        124
    );
    assert_eq!(std::mem::size_of::<PhuxArrangementGroup>(), 56);
    assert_eq!(std::mem::size_of::<PhuxArrangementNode>(), 56);
    assert_eq!(std::mem::size_of::<PhuxCatalogTerminal>(), 80);
    // The kinds fill former padding: no size or other offset moved.
    assert_eq!(std::mem::offset_of!(PhuxArrangementNode, resource_kind), 52);
    assert_eq!(std::mem::offset_of!(PhuxCatalogTerminal, resource_kind), 44);
    assert_eq!(std::mem::offset_of!(PhuxCatalogTerminal, title), 48);
    assert_eq!(std::mem::size_of::<PhuxArrangementMutation>(), 136);
    assert_eq!(
        std::mem::offset_of!(PhuxArrangementMutation, path_bits),
        128
    );
    let mut client = harness();
    let mut info = PhuxArrangementInfo::default();
    // SAFETY: deliberate null/invalid headers, otherwise owned disjoint values.
    unsafe {
        assert_eq!(
            phux_client_arrangement_info(std::ptr::null(), &raw mut info),
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(
            phux_client_arrangement_info(&raw const *client, std::ptr::null_mut()),
            PhuxClientResult::InvalidArgument
        );
        info.size -= 1;
        assert_eq!(
            phux_client_arrangement_info(&raw const *client, &raw mut info),
            PhuxClientResult::InvalidArgument
        );
        info = PhuxArrangementInfo::default();
        info.version += 1;
        assert_eq!(
            phux_client_arrangement_info(&raw const *client, &raw mut info),
            PhuxClientResult::InvalidArgument
        );
        info = PhuxArrangementInfo::default();
        assert_eq!(
            phux_client_arrangement_info(&raw const *client, &raw mut info),
            PhuxClientResult::Ok
        );
        assert_eq!(info.terminal_count, 3);
        assert_eq!(
            phux_client_arrangement_mutate(&raw mut *client, std::ptr::null()),
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(
            phux_client_arrangement_refresh(&raw mut *client, 1),
            PhuxClientResult::Ok
        );
        assert_eq!(
            phux_client_disconnect(&raw mut *client),
            PhuxClientResult::Ok
        );
    }
    assert_eq!(client.inner.arrangement.status, 4);
    assert!(client.inner.arrangement.pending.is_none());
}
