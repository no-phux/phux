//! Which registry entry a target names, and which name a new one is saved
//! under.
//!
//! A typed `:PORT` names a server, so two servers on one machine are two
//! hosts: the first keeps the bare name and a second one saved at another
//! port is `HOST-PORT` (`mini` and `mini-9788`). A target with no port, or
//! with the port the bare entry already dials, means the bare entry, so a
//! registry with one server per machine reads and writes as before.

use phux_config_core::RemoteConfigEntry;

use super::RemoteTarget;

/// The registry entry that describes `target`.
///
/// Matched as `user@host-PORT` or `host-PORT` when a port was typed, then
/// exact `user@host`, then the bare host (what `phux host add` registers),
/// then any entry whose endpoint addresses that host at the typed port.
#[must_use]
pub fn find_entry<'a>(
    entries: &'a [RemoteConfigEntry],
    target: &RemoteTarget,
) -> Option<&'a RemoteConfigEntry> {
    find_entry_by(entries, target, |entry| (&entry.name, &entry.endpoint))
}

/// [`find_entry`] over any registry row type, given its `(name, endpoint)`.
#[must_use]
pub fn find_entry_by<'a, E>(
    entries: &'a [E],
    target: &RemoteTarget,
    name_and_endpoint: impl Fn(&E) -> (&str, &str),
) -> Option<&'a E> {
    let name = target.registry_name();
    let named = |wanted: &str| {
        entries
            .iter()
            .find(|entry| name_and_endpoint(entry).0 == wanted)
    };
    let port_named = target.port.and_then(|port| {
        named(&format!("{name}-{port}")).or_else(|| named(&format!("{}-{port}", target.host)))
    });
    port_named
        .or_else(|| named(&name))
        .or_else(|| named(&target.host))
        .or_else(|| {
            entries.iter().find(|entry| {
                let endpoint = name_and_endpoint(entry).1;
                endpoint_host(endpoint).as_deref() == Some(&target.host)
                    && !other_port(endpoint, target.port)
            })
        })
}

/// The name to save a new entry for `base` typed with `port` under, beside
/// `entries`: `base`, unless an entry by that name dials another port than
/// the typed one, which is another server, so `base-PORT` is.
#[must_use]
pub fn registration_name_by<E>(
    entries: &[E],
    base: &str,
    port: Option<u16>,
    name_and_endpoint: impl Fn(&E) -> (&str, &str),
) -> String {
    let taken = entries.iter().any(|entry| {
        let (name, endpoint) = name_and_endpoint(entry);
        name == base && other_port(endpoint, port)
    });
    match port {
        Some(port) if taken => format!("{base}-{port}"),
        _ => base.to_owned(),
    }
}

/// Whether `endpoint` names a port and it is not the typed `port`.
fn other_port(endpoint: &str, port: Option<u16>) -> bool {
    port.is_some_and(|port| endpoint_port(endpoint).is_some_and(|saved| saved != port))
}

/// The host an endpoint URI addresses, unbracketed; `None` when it has none.
#[must_use]
pub fn endpoint_host(endpoint: &str) -> Option<String> {
    let rest = endpoint.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split_once(']').map(|(host, _)| host)?
    } else if authority.matches(':').count() > 1 {
        authority
    } else {
        authority
            .split_once(':')
            .map_or(authority, |(host, _)| host)
    };
    (!host.is_empty()).then(|| host.to_owned())
}

/// The port an endpoint URI names; `None` when it names none.
#[must_use]
pub fn endpoint_port(endpoint: &str) -> Option<u16> {
    let rest = endpoint.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    let port = authority.rsplit_once(':').map(|(_, port)| port)?;
    port.parse::<u16>().ok().filter(|port| *port != 0)
}
