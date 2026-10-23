//! `phux host discover`: the machines on this computer's tailnet, and
//! which of them are already registered.

use std::process::ExitCode;

use phux_config::tailnet::{self, Tailnet, TailnetPeer, TailnetState};

use super::super::json_err::{self, CliError, codes};
use super::super::remote::{self, RemoteEntry};
use super::super::remote_target::endpoint_host;
use super::add::registry_failure;

/// `phux host discover [--login]`.
pub(super) fn run_discover(login: bool, json: bool) -> ExitCode {
    let tailnet = if login {
        match tailnet::sign_in() {
            Ok(tailnet) => tailnet,
            Err(error) => {
                let err = CliError::new(codes::TAILNET, error.message, error.remedy);
                return json_err::emit(json, &err, 1);
            }
        }
    } else {
        tailnet::discover()
    };
    let registered = match remote::load_registry() {
        Ok(entries) => entries,
        Err(err) => return json_err::emit(json, &registry_failure(err), 1),
    };
    if json {
        return crate::output::json(&document(&tailnet, &registered));
    }
    out!("{}", render(&tailnet, &registered));
    ExitCode::SUCCESS
}

/// The registered remote that reaches `peer`: its endpoint, kept direct
/// route, or ssh destination names it. A registry name alone proves
/// nothing; another machine may carry it.
pub(super) fn registered_as<'a>(peer: &TailnetPeer, entries: &'a [RemoteEntry]) -> Option<&'a str> {
    let names_peer = |entry: &RemoteEntry| {
        [Some(&entry.endpoint), entry.direct.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|endpoint| endpoint_host(endpoint))
            .chain(
                entry
                    .ssh
                    .as_deref()
                    .map(super::super::enroll::destination_host),
            )
            .any(|host| peer.is_named_by(&host))
    };
    entries
        .iter()
        .find(|entry| names_peer(entry))
        .map(|entry| entry.name.as_str())
}

/// `host discover --json`, `schema_version` 1.
pub(super) fn document(tailnet: &Tailnet, entries: &[RemoteEntry]) -> serde_json::Value {
    let peers: Vec<_> = tailnet
        .peers
        .iter()
        .map(|peer| {
            serde_json::json!({
                "name": peer.name,
                "dns_name": peer.dns_name,
                "host_name": peer.host_name,
                "os": peer.os,
                "online": peer.online,
                "addresses": peer.addresses.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "tailscale_ssh": peer.tailscale_ssh,
                "supported": peer.supported(),
                "ssh_host": peer.ssh_host(),
                "registered": registered_as(peer, entries),
            })
        })
        .collect();
    serde_json::json!({
        "schema_version": 1,
        "tailscale": {
            "state": tailnet.state.as_str(),
            "cli": tailnet.cli.as_ref().map(|cli| cli.display().to_string()),
            "auth_url": tailnet.auth_url,
            "tailnet": tailnet.name,
            "self": tailnet.self_name,
        },
        "peers": peers,
    })
}

/// What the client's state means for the person reading the table.
const fn state_line(state: TailnetState) -> &'static str {
    match state {
        TailnetState::Running => "connected",
        TailnetState::Starting => "connecting; run this again in a moment",
        TailnetState::Stopped => {
            "signed in but disconnected; `phux host discover --login` reconnects it"
        }
        TailnetState::NeedsLogin => "not signed in; `phux host discover --login` starts a sign-in",
        TailnetState::NeedsMachineAuth => {
            "waiting for a tailnet admin to approve this device in the admin console"
        }
        TailnetState::NotRunning => {
            "installed but not running; start the Tailscale app or tailscaled"
        }
        TailnetState::NotInstalled => {
            "not installed; get it from https://tailscale.com/download to find your machines"
        }
    }
}

pub(super) fn render(tailnet: &Tailnet, entries: &[RemoteEntry]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "Tailscale: {}", state_line(tailnet.state));
    if let Some(url) = &tailnet.auth_url {
        let _ = writeln!(out, "Sign in at {url}");
    }
    if tailnet.state != TailnetState::Running {
        return out;
    }
    if let Some(name) = &tailnet.name {
        let _ = writeln!(out, "Tailnet: {name}");
    }
    let peers: Vec<&TailnetPeer> = tailnet
        .peers
        .iter()
        .filter(|peer| peer.supported())
        .collect();
    if peers.is_empty() {
        let _ = writeln!(out, "No other Linux or macOS machines on the tailnet.");
        return out;
    }
    let rows: Vec<[String; 5]> = peers
        .iter()
        .map(|peer| {
            [
                peer.name.clone(),
                peer.os.clone(),
                if peer.online { "online" } else { "offline" }.to_owned(),
                peer.ssh_host(),
                registered_as(peer, entries).unwrap_or("-").to_owned(),
            ]
        })
        .collect();
    let header = ["NAME", "OS", "STATE", "SSH TO", "REGISTERED"].map(str::to_owned);
    let widths: Vec<usize> = (0..4)
        .map(|column| {
            std::iter::once(&header)
                .chain(&rows)
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let _ = writeln!(out);
    for row in std::iter::once(&header).chain(&rows) {
        let _ = writeln!(
            out,
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            row[4],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
        );
    }
    if let Some(peer) = peers
        .iter()
        .find(|peer| peer.online && registered_as(peer, entries).is_none())
    {
        let _ = writeln!(
            out,
            "\nSet one up, installing phux there if needed:\n  phux host add {} --name {} --install --accept-new-host-key",
            peer.ssh_host(),
            peer.name
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phux_config::tailnet::{Tailnet, TailnetPeer, TailnetState};

    use super::super::super::remote::RemoteEntry;
    use super::{document, registered_as, render};

    fn peer(name: &str, os: &str, online: bool) -> TailnetPeer {
        TailnetPeer {
            name: name.to_owned(),
            dns_name: Some(format!("{name}.tail9a8b.ts.net")),
            host_name: name.to_owned(),
            os: os.to_owned(),
            online,
            addresses: vec![format!("100.70.0.{}", name.len()).parse().expect("ip")],
            tailscale_ssh: false,
        }
    }

    fn entry(name: &str, endpoint: &str, ssh: Option<&str>) -> RemoteEntry {
        RemoteEntry {
            index: 0,
            name: name.to_owned(),
            endpoint: endpoint.to_owned(),
            token_file: None,
            cert_fingerprint: None,
            tls_server_name: None,
            session: None,
            ssh: ssh.map(str::to_owned),
            direct: None,
            remote_phux: None,
            client_cert: None,
            client_key: None,
        }
    }

    fn running(peers: Vec<TailnetPeer>) -> Tailnet {
        Tailnet {
            state: TailnetState::Running,
            cli: Some(PathBuf::from("/bin/tailscale")),
            auth_url: None,
            name: Some("me@example.com".to_owned()),
            self_name: Some("studio.tail9a8b.ts.net".to_owned()),
            peers,
        }
    }

    #[test]
    fn a_peer_is_registered_by_the_route_that_reaches_it_not_by_name() {
        let mini = peer("mini", "linux", true);
        let entries = [
            entry("mini", "quic://192.168.1.9:8788", None),
            entry(
                "lab",
                "ssh://me@mini.tail9a8b.ts.net",
                Some("me@mini.tail9a8b.ts.net"),
            ),
        ];
        assert_eq!(registered_as(&mini, &entries), Some("lab"));
        assert_eq!(
            registered_as(&mini, &entries[..1]),
            None,
            "same name, other machine"
        );
        let by_address = [entry("m", "quic://100.70.0.4:8788", None)];
        assert_eq!(registered_as(&mini, &by_address), Some("m"));
    }

    #[test]
    fn the_document_carries_what_the_desktop_reads() {
        let tailnet = running(vec![
            peer("mini", "linux", true),
            peer("phone", "iOS", true),
        ]);
        let doc = document(&tailnet, &[]);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["tailscale"]["state"], "running");
        assert_eq!(doc["tailscale"]["tailnet"], "me@example.com");
        assert_eq!(doc["peers"][0]["ssh_host"], "mini.tail9a8b.ts.net");
        assert_eq!(doc["peers"][0]["supported"], true);
        assert_eq!(doc["peers"][0]["registered"], serde_json::Value::Null);
        assert_eq!(doc["peers"][1]["supported"], false);
    }

    #[test]
    fn the_table_lists_supported_machines_and_suggests_one() {
        let tailnet = running(vec![
            peer("mini", "linux", true),
            peer("phone", "iOS", true),
        ]);
        let text = render(&tailnet, &[]);
        assert!(text.contains("mini.tail9a8b.ts.net"), "{text}");
        assert!(!text.contains("phone"), "{text}");
        assert!(
            text.contains(
                "phux host add mini.tail9a8b.ts.net --name mini --install --accept-new-host-key"
            ),
            "{text}"
        );
        let signed_out = Tailnet {
            state: TailnetState::NeedsLogin,
            auth_url: Some("https://login.tailscale.com/a/x".to_owned()),
            ..running(Vec::new())
        };
        let text = render(&signed_out, &[]);
        assert!(
            text.contains("--login") && text.contains("Sign in at https://login.tailscale.com/a/x"),
            "{text}"
        );
    }
}
