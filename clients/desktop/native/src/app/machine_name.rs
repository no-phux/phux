//! What a person calls a machine. A machine saved by address reads as the
//! host name the system resolver gives it, so a tailnet IP shows as the
//! MagicDNS name (`100.79.155.27` as `mac`). Lookups run off the UI thread,
//! once per address per process; until one answers, the address shows.

use std::collections::HashMap;
use std::net::IpAddr;
use std::process::Command;
use std::sync::{Mutex, PoisonError};

static NAMES: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

/// The name to show for machine `host`, or `None` for this Mac.
pub(crate) fn label(host: Option<&str>) -> String {
    host.map_or_else(
        || crate::platform::local_machine_label().to_owned(),
        display,
    )
}

/// The resolved name for `host` when it is an address that has one.
pub(crate) fn display(host: &str) -> String {
    NAMES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|names| names.get(host).cloned().flatten())
        .unwrap_or_else(|| host.to_owned())
}

/// Addresses among `hosts` not looked up yet.
pub(crate) fn unresolved<'a>(hosts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let names = NAMES.lock().unwrap_or_else(PoisonError::into_inner);
    hosts
        .into_iter()
        .filter(|host| host.parse::<IpAddr>().is_ok())
        .filter(|host| {
            names
                .as_ref()
                .is_none_or(|names| !names.contains_key(*host))
        })
        .map(str::to_owned)
        .collect()
}

/// Look up each address and remember the answer, found or not. Blocks;
/// call it from a background task. True when any address gained a name.
pub(crate) fn resolve(addresses: Vec<String>) -> bool {
    let found: Vec<_> = addresses
        .into_iter()
        .map(|address| {
            let name = reverse(&address).and_then(|fqdn| short(&fqdn));
            (address, name)
        })
        .collect();
    let named = found.iter().any(|(_, name)| name.is_some());
    NAMES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .extend(found);
    named
}

/// The system resolver's name for `address`: mDNSResponder on macOS, NSS
/// elsewhere, so MagicDNS and `/etc/hosts` both answer.
fn reverse(address: &str) -> Option<String> {
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("/usr/bin/dscacheutil", &["-q", "host", "-a", "ip_address"])
    } else {
        ("getent", &["hosts"])
    };
    let output = Command::new(program)
        .args(args)
        .arg(address)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if cfg!(target_os = "macos") {
        dscacheutil_name(&text)
    } else {
        getent_name(&text)
    }
}

fn dscacheutil_name(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("name:"))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

fn getent_name(text: &str) -> Option<String> {
    text.split_whitespace().nth(1).map(str::to_owned)
}

/// A host name's first label: `mac.tail1354da.ts.net.` is `mac`. An answer
/// that is itself an address names nothing.
fn short(fqdn: &str) -> Option<String> {
    let fqdn = fqdn.trim_end_matches('.');
    if fqdn.parse::<IpAddr>().is_ok() {
        return None;
    }
    fqdn.split('.')
        .next()
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_answers_read_as_short_names() {
        let mac = "name: mac.tail1354da.ts.net\nalias: 27.155.79.100.in-addr.arpa \nip_address: 100.79.155.27\n";
        assert_eq!(
            dscacheutil_name(mac).as_deref(),
            Some("mac.tail1354da.ts.net")
        );
        assert_eq!(dscacheutil_name(""), None);
        assert_eq!(
            getent_name("100.79.155.27   mini.tail1354da.ts.net\n").as_deref(),
            Some("mini.tail1354da.ts.net")
        );
        assert_eq!(short("mac.tail1354da.ts.net.").as_deref(), Some("mac"));
        assert_eq!(short("studio").as_deref(), Some("studio"));
        assert_eq!(short("100.79.155.27"), None);
    }

    #[test]
    fn only_unlooked_addresses_are_resolved_and_names_fall_back() {
        let address = "192.0.2.77";
        assert_eq!(unresolved(["mini", address]), vec![address.to_owned()]);
        NAMES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(address.into(), Some("studio".into()));
        assert!(unresolved([address]).is_empty());
        assert_eq!(display(address), "studio");
        assert_eq!(label(Some("mini")), "mini");
        assert_eq!(label(None), crate::platform::local_machine_label());
    }
}
