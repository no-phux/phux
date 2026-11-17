//! Whether a tailnet peer is one of this machine's owner's own devices, on
//! the local Tailscale client's word (`[policy] tailnet-owner`,
//! `docs/config.md#tailnet-owner-devices`).
//!
//! Both sides come from `tailscale whois --json`, run through the CLI
//! [`crate::overlay::tailscale_cli`] finds: that CLI is tailscaled's own
//! `LocalAPI` client and already knows every platform's way to reach it (the
//! Linux socket, the macOS app's). The owner is whoever the tailnet address
//! the connection arrived on belongs to, so nothing a peer sends names it.
//! Every failure refuses.

use std::net::IpAddr;
use std::time::Duration;

use crate::overlay::{is_tailnet, run_tailscale, tailscale_cli};

/// How long one `tailscale whois` may run: it answers from tailscaled's
/// memory, so a slow one means a wedged daemon, which refuses.
const WHOIS_DEADLINE: Duration = Duration::from_secs(2);

/// What `tailscale whois` says about one tailnet address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Whois {
    /// The tailnet user the node belongs to (`Node.User`).
    pub user_id: u64,
    /// That user's login name (`UserProfile.LoginName`).
    pub login: String,
    /// The node carries ACL tags: it belongs to no person.
    pub tagged: bool,
    /// The node was shared in from another tailnet.
    pub shared: bool,
}

/// Parse `tailscale whois --json` output.
///
/// # Errors
///
/// Output that is not that document, or one whose node and profile name
/// different users.
pub fn parse_whois(out: &str) -> Result<Whois, String> {
    let doc: serde_json::Value =
        serde_json::from_str(out).map_err(|err| format!("whois output is not JSON: {err}"))?;
    let user_id = doc
        .pointer("/Node/User")
        .and_then(serde_json::Value::as_u64)
        .ok_or("whois names no node user")?;
    if doc
        .pointer("/UserProfile/ID")
        .and_then(serde_json::Value::as_u64)
        != Some(user_id)
    {
        return Err("whois node and profile name different users".to_owned());
    }
    let login = doc
        .pointer("/UserProfile/LoginName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let present = |pointer: &str| {
        doc.pointer(pointer).is_some_and(|value| match value {
            serde_json::Value::Null | serde_json::Value::Bool(false) => false,
            serde_json::Value::Array(items) => !items.is_empty(),
            serde_json::Value::Number(n) => n.as_u64() != Some(0),
            serde_json::Value::String(s) => !s.is_empty(),
            _ => true,
        })
    };
    Ok(Whois {
        user_id,
        login,
        tagged: present("/Node/Tags"),
        shared: present("/Node/Sharer"),
    })
}

/// Whether `caller` is one of `owner`'s own devices: the same tailnet user
/// by id and login, neither node tagged or shared in.
#[must_use]
pub fn same_owner(owner: &Whois, caller: &Whois) -> bool {
    let personal =
        |node: &Whois| node.user_id != 0 && !node.login.is_empty() && !node.tagged && !node.shared;
    personal(owner)
        && personal(caller)
        && owner.user_id == caller.user_id
        && owner.login == caller.login
}

/// Ask the local Tailscale client about `ip`.
///
/// # Errors
///
/// No Tailscale CLI, a failed or slow run, or output [`parse_whois`] refuses.
pub fn whois(ip: IpAddr) -> Result<Whois, String> {
    let cli = tailscale_cli().ok_or("no Tailscale CLI")?;
    let out = run_tailscale(
        cli.as_os_str(),
        &["whois", "--json", &ip.to_string()],
        WHOIS_DEADLINE,
    )
    .ok_or_else(|| format!("tailscale whois {ip} failed"))?;
    parse_whois(&out)
}

/// The owner's login when `peer` is one of the owner's own devices.
///
/// The owner is whoever this machine's tailnet address `local` (the address
/// the connection arrived on) belongs to. `whois` is the lookup, injected
/// for tests.
///
/// # Errors
///
/// Why the peer is refused: an address outside the tailnet, a lookup that
/// failed, or a different, tagged, or shared-in node.
pub fn owner_device(
    local: IpAddr,
    peer: IpAddr,
    whois: impl Fn(IpAddr) -> Result<Whois, String>,
) -> Result<String, String> {
    let (local, peer) = (local.to_canonical(), peer.to_canonical());
    if !is_tailnet(local) || !is_tailnet(peer) {
        return Err("not a tailnet connection".to_owned());
    }
    let owner = whois(local)?;
    let caller = whois(peer)?;
    if !same_owner(&owner, &caller) {
        return Err("not the owner's own untagged device".to_owned());
    }
    Ok(caller.login)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = r#"{"Node":{"ID":1,"User":27,"Tags":null,"Sharer":null},
        "UserProfile":{"ID":27,"LoginName":"me@example.com","DisplayName":"Me"},"CapMap":{}}"#;
    const PHONE: &str = r#"{"Node":{"ID":2,"User":27},
        "UserProfile":{"ID":27,"LoginName":"me@example.com"}}"#;
    const OTHER_USER: &str = r#"{"Node":{"ID":3,"User":99},
        "UserProfile":{"ID":99,"LoginName":"them@example.com"}}"#;
    const TAGGED: &str = r#"{"Node":{"ID":4,"User":27,"Tags":["tag:ci"]},
        "UserProfile":{"ID":27,"LoginName":"me@example.com"}}"#;
    const SHARED: &str = r#"{"Node":{"ID":5,"User":27,"Sharer":99},
        "UserProfile":{"ID":27,"LoginName":"me@example.com"}}"#;
    const MISMATCHED: &str = r#"{"Node":{"ID":6,"User":27},
        "UserProfile":{"ID":99,"LoginName":"me@example.com"}}"#;

    const LOCAL: &str = "100.64.0.1";
    const PEER: &str = "100.64.0.2";

    fn decide(local: &str, peer: &str, owner: &str, caller: &str) -> Result<String, String> {
        let local_ip: IpAddr = local.parse().unwrap();
        let (owner, caller) = (owner.to_owned(), caller.to_owned());
        owner_device(local_ip, peer.parse().unwrap(), move |ip| {
            parse_whois(if ip == local_ip { &owner } else { &caller })
        })
    }

    #[test]
    fn admits_the_owners_own_device() {
        assert_eq!(
            decide(LOCAL, PEER, OWNER, PHONE).as_deref(),
            Ok("me@example.com")
        );
    }

    #[test]
    fn refuses_another_user_a_tagged_node_and_a_shared_node() {
        for caller in [OTHER_USER, TAGGED, SHARED, MISMATCHED, "not json", "{}"] {
            assert!(decide(LOCAL, PEER, OWNER, caller).is_err(), "{caller}");
        }
    }

    #[test]
    fn a_tagged_server_has_no_owner() {
        assert!(decide(LOCAL, PEER, TAGGED, PHONE).is_err());
    }

    #[test]
    fn refuses_a_failed_lookup_and_addresses_off_the_tailnet() {
        let failed = owner_device(LOCAL.parse().unwrap(), PEER.parse().unwrap(), |_| {
            Err("tailscaled is not running".to_owned())
        });
        assert!(failed.is_err());
        assert!(decide("192.168.1.2", PEER, OWNER, PHONE).is_err());
        assert!(decide(LOCAL, "192.168.1.3", OWNER, PHONE).is_err());
        assert!(decide("0.0.0.0", PEER, OWNER, PHONE).is_err());
    }

    #[test]
    fn a_v4_mapped_peer_is_the_v4_address() {
        assert!(decide(LOCAL, "::ffff:100.64.0.2", OWNER, PHONE).is_ok());
    }
}
