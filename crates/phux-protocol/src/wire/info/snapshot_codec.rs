//! The snapshot codec: the counted lists, then the trailing host inventory, session facets, and listeners.

use super::{
    DecodeError, Decoder, Encoder, HostInventory, HostSessionInfo, SESSION_FACET_KEEP_EMPTY,
    SatelliteHost, SessionId, SessionInfo, SessionSnapshot, WindowId, decode_list, decode_list_len,
    decode_option_str, decode_option_terminal_id, decode_resource_facets, decode_session_info,
    decode_snapshot_extension, decode_terminal_id, decode_terminal_info, decode_window_info,
    encode_list, encode_list_len, encode_option_str, encode_option_terminal_id,
    encode_resource_facets, encode_session_info, encode_snapshot_extension, encode_terminal_id,
    encode_terminal_info, encode_window_info,
};

pub(in crate::wire) fn encode_session_snapshot(snap: &SessionSnapshot, enc: &mut Encoder<'_>) {
    encode_list(&snap.sessions, enc, encode_session_info);
    encode_list(&snap.windows, enc, encode_window_info);
    encode_list(&snap.resources, enc, encode_terminal_info);
    enc.write_u32_be(snap.focused_session.get());
    enc.write_u32_be(snap.focused_window.get());
    encode_terminal_id(&snap.focused_resource, enc);
    let extension = encode_snapshot_extension(&snap.sessions, &snap.resources, snap.journal_head());
    let extension_follows = !extension.is_empty();
    let session_rows = snap.sessions.iter().filter(|s| s.keep_empty).count();
    let listeners_follow = snap.listeners().is_some() || extension_follows;
    let session_facets_follow = session_rows > 0 || listeners_follow;
    let hosts_follow = !snap.hosts().is_empty() || session_facets_follow;
    encode_resource_facets(&snap.resources, hosts_follow, enc);
    encode_host_inventory(snap.hosts(), session_facets_follow, enc);
    encode_session_facets(&snap.sessions, session_rows, listeners_follow, enc);
    encode_listeners(snap.listeners(), extension_follows, enc);
    if extension_follows {
        enc.write_bytes(&extension);
    }
}

/// Store an inventory canonically: `None` when empty, so a snapshot built
/// with no hosts and one decoded without the trailing list compare equal.
pub(super) fn boxed_hosts(hosts: Vec<HostInventory>) -> Option<Box<[HostInventory]>> {
    (!hosts.is_empty()).then(|| hosts.into_boxed_slice())
}

/// Write the trailing host inventory, or nothing when it is empty and no
/// later element needs it as an anchor.
pub(super) fn encode_host_inventory(
    hosts: &[HostInventory],
    more_follow: bool,
    enc: &mut Encoder<'_>,
) {
    if hosts.is_empty() && !more_follow {
        return;
    }
    encode_list(hosts, enc, |row, enc| {
        enc.write_record(|enc| {
            enc.write_str(row.host.as_str());
            encode_option_str(row.unreachable.as_deref(), enc);
            encode_list(&row.sessions, enc, encode_host_session);
        });
    });
}

pub(super) fn encode_host_session(session: &HostSessionInfo, enc: &mut Encoder<'_>) {
    enc.write_record(|enc| {
        enc.write_u32_be(session.id.get());
        enc.write_str(&session.name);
        enc.write_i64_be(session.created_at_unix_secs);
        enc.write_u16_be(session.window_count);
        enc.write_u16_be(session.pane_count);
        enc.write_u16_be(session.attached_client_count);
        encode_option_terminal_id(session.active_resource.as_ref(), enc);
    });
}

/// Read the trailing host-session inventory if the enclosing field has bytes
/// left; an absent list is an empty inventory.
pub(super) fn decode_host_inventory(
    dec: &mut Decoder<'_>,
) -> Result<Vec<HostInventory>, DecodeError> {
    if dec.at_body_end() {
        return Ok(Vec::new());
    }
    decode_list(dec, |dec| {
        dec.read_record(|dec| {
            Ok(HostInventory {
                host: SatelliteHost::new(dec.read_str()?),
                unreachable: decode_option_str(dec)?.map(str::to_owned),
                sessions: decode_list(dec, decode_host_session)?,
            })
        })
    })
}

pub(super) fn decode_host_session(dec: &mut Decoder<'_>) -> Result<HostSessionInfo, DecodeError> {
    dec.read_record(decode_host_session_body)
}

pub(super) fn decode_host_session_body(
    dec: &mut Decoder<'_>,
) -> Result<HostSessionInfo, DecodeError> {
    let id = SessionId::new(dec.read_u32_be()?);
    let name = dec.read_str()?.to_owned();
    let created_at_unix_secs = dec.read_i64_be()?;
    let window_count = dec.read_u16_be()?;
    let pane_count = dec.read_u16_be()?;
    let attached_client_count = dec.read_u16_be()?;
    let active_resource = decode_option_terminal_id(dec)?;
    Ok(HostSessionInfo {
        id,
        name,
        created_at_unix_secs,
        window_count,
        pane_count,
        attached_client_count,
        active_resource,
    })
}

/// Write the trailing session-facet list, or nothing when it is empty and no
/// later element needs it as an anchor.
pub(super) fn encode_session_facets(
    sessions: &[SessionInfo],
    rows: usize,
    more_follow: bool,
    enc: &mut Encoder<'_>,
) {
    if rows == 0 && !more_follow {
        return;
    }
    encode_list_len(rows, enc);
    for session in sessions.iter().filter(|s| s.keep_empty) {
        enc.write_record(|enc| {
            enc.write_u32_be(session.id.get());
            enc.write_u8(SESSION_FACET_KEEP_EMPTY);
        });
    }
}

/// Write the trailing listeners JSON; an unset report writes nothing, or an
/// absent marker when the extension block follows.
pub(super) fn encode_listeners(
    report: Option<&crate::wire::listeners::RemoteListenersReport>,
    more_follow: bool,
    enc: &mut Encoder<'_>,
) {
    let Some(report) = report else {
        if more_follow {
            encode_option_str(None, enc);
        }
        return;
    };
    let json = report.to_json();
    encode_option_str(Some(json.as_str()), enc);
}

/// Read the trailing remote-listeners JSON if bytes remain.
pub(super) fn decode_listeners(
    dec: &mut Decoder<'_>,
) -> Result<Option<crate::wire::listeners::RemoteListenersReport>, DecodeError> {
    if dec.at_body_end() {
        return Ok(None);
    }
    Ok(decode_option_str(dec)?.and_then(crate::wire::listeners::RemoteListenersReport::from_json))
}

/// Read the trailing session-facet list if bytes remain, joining each row
/// onto its `sessions` entry by id. Unknown flag bits are ignored.
pub(super) fn decode_session_facets(
    dec: &mut Decoder<'_>,
    sessions: &mut [SessionInfo],
) -> Result<(), DecodeError> {
    if dec.at_body_end() {
        return Ok(());
    }
    let rows = decode_list_len(dec)?;
    for _ in 0..rows {
        let (id, flags) =
            dec.read_record(|dec| Ok((SessionId::new(dec.read_u32_be()?), dec.read_u8()?)))?;
        if let Some(session) = sessions.iter_mut().find(|s| s.id == id) {
            session.keep_empty = flags & SESSION_FACET_KEEP_EMPTY != 0;
        }
    }
    Ok(())
}

pub(in crate::wire) fn decode_session_snapshot(
    dec: &mut Decoder<'_>,
) -> Result<SessionSnapshot, DecodeError> {
    let mut sessions = decode_list(dec, decode_session_info)?;
    let windows = decode_list(dec, decode_window_info)?;
    let mut resources = decode_list(dec, decode_terminal_info)?;
    let focused_session = SessionId::new(dec.read_u32_be()?);
    let focused_window = WindowId::new(dec.read_u32_be()?);
    let focused_resource = decode_terminal_id(dec)?;
    decode_resource_facets(dec, &mut resources)?;
    let hosts = decode_host_inventory(dec)?;
    decode_session_facets(dec, &mut sessions)?;
    let listeners = decode_listeners(dec)?;
    let journal_head = decode_snapshot_extension(dec, &mut sessions, &mut resources)?;
    let mut snapshot = SessionSnapshot {
        sessions,
        windows,
        resources,
        focused_session,
        focused_window,
        focused_resource,
        trail: None,
    };
    if !hosts.is_empty() {
        snapshot.set_hosts(hosts);
    }
    if let Some(listeners) = listeners {
        snapshot.set_listeners(Some(listeners));
    }
    if journal_head.is_some() {
        snapshot.set_journal_head(journal_head);
    }
    Ok(snapshot)
}
