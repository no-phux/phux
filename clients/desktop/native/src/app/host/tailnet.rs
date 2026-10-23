//! The machines on this computer's tailnet, from `phux host discover`.

use std::path::Path;

use super::{args, project_error, run_output};

/// A machine on this computer's tailnet, from `phux host discover --json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailnetPeer {
    pub name: String,
    pub os: String,
    pub online: bool,
    /// What `phux host add` is given: the `MagicDNS` name.
    pub ssh_host: String,
    /// The registered machine already reaching it.
    pub registered: Option<String>,
}

/// The local Tailscale client's state and the machines phux can run on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tailnet {
    /// `running`, `needs_login`, `stopped`, `not_installed`, ...
    pub state: String,
    pub auth_url: Option<String>,
    pub tailnet: Option<String>,
    pub peers: Vec<TailnetPeer>,
}

impl Tailnet {
    pub fn running(&self) -> bool {
        self.state == "running"
    }

    /// The peer `target` (an ssh destination, maybe with `user@`) names by
    /// its full `MagicDNS` name. A short name is not enough: ssh may resolve
    /// it through its config or the LAN to a machine the tailnet does not
    /// vouch for.
    pub fn peer_for(&self, target: &str) -> Option<&TailnetPeer> {
        let host = target
            .trim()
            .rsplit_once('@')
            .map_or(target.trim(), |(_, host)| host);
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.peers.iter().find(|peer| peer.ssh_host == host)
    }
}

/// `phux host discover --json`; `login` first signs Tailscale in or brings
/// it up, returning once a sign-in link is known.
pub fn discover_tailnet(phux: &Path, login: bool) -> Result<Tailnet, String> {
    let mut args = args(&["host", "discover", "--json"]);
    if login {
        args.push("--login".to_owned());
    }
    let output = run_output(phux, args).map_err(|error| cli_error(&error))?;
    parse_tailnet(&output)
}

fn parse_tailnet(output: &[u8]) -> Result<Tailnet, String> {
    let document: serde_json::Value = serde_json::from_slice(output)
        .map_err(|_| "Tailnet discovery returned an invalid response.".to_owned())?;
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let tailscale = document
        .get("tailscale")
        .ok_or_else(|| "Tailnet discovery returned no Tailscale state.".to_owned())?;
    let peers = document
        .get("peers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|peer| peer.get("supported").and_then(serde_json::Value::as_bool) == Some(true))
        .filter_map(|peer| {
            Some(TailnetPeer {
                name: text(peer, "name")?,
                os: text(peer, "os").unwrap_or_default(),
                online: peer.get("online").and_then(serde_json::Value::as_bool) == Some(true),
                ssh_host: text(peer, "ssh_host")?,
                registered: text(peer, "registered"),
            })
        })
        .collect();
    Ok(Tailnet {
        state: text(tailscale, "state").unwrap_or_else(|| "not_running".to_owned()),
        auth_url: text(tailscale, "auth_url").filter(|url| url.starts_with("https://")),
        tailnet: text(tailscale, "tailnet"),
        peers,
    })
}

/// The CLI's JSON error reads as its message and remedy; an older CLI
/// without `host discover` is told to update.
fn cli_error(error: &str) -> String {
    if error.contains("discover") && (error.contains("unrecognized") || error.contains("unknown")) {
        return "Finding machines on your tailnet needs a newer phux CLI. Update it from Settings > Updates.".into();
    }
    project_error(error.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{TailnetPeer, cli_error, parse_tailnet};

    #[test]
    fn tailnet_discovery_keeps_machines_phux_runs_on() {
        let json = br#"{"schema_version":1,
            "tailscale":{"state":"running","cli":"/x","auth_url":null,"tailnet":"me@example.com","self":"mac.ts.net"},
            "peers":[
              {"name":"mini","dns_name":"mini.tail9a8b.ts.net","os":"linux","online":true,"supported":true,
               "ssh_host":"mini.tail9a8b.ts.net","registered":"mini","tailscale_ssh":true,"addresses":[]},
              {"name":"phone","os":"iOS","online":true,"supported":false,"ssh_host":"phone.ts.net","registered":null}
            ]}"#;
        let tailnet = parse_tailnet(json).unwrap();
        assert!(tailnet.running());
        assert_eq!(tailnet.tailnet.as_deref(), Some("me@example.com"));
        assert_eq!(
            tailnet.peers,
            vec![TailnetPeer {
                name: "mini".into(),
                os: "linux".into(),
                online: true,
                ssh_host: "mini.tail9a8b.ts.net".into(),
                registered: Some("mini".into()),
            }]
        );
        for target in [
            "mini.tail9a8b.ts.net",
            "me@mini.tail9a8b.ts.net",
            "MINI.tail9a8b.ts.net.",
        ] {
            assert!(tailnet.peer_for(target).is_some(), "{target}");
        }
        for target in ["mini", "me@mini", "box"] {
            assert!(tailnet.peer_for(target).is_none(), "{target}");
        }

        let signing_in = parse_tailnet(
            br#"{"tailscale":{"state":"needs_login","auth_url":"https://login.tailscale.com/a/x"},"peers":[]}"#,
        )
        .unwrap();
        assert!(!signing_in.running());
        assert_eq!(
            signing_in.auth_url.as_deref(),
            Some("https://login.tailscale.com/a/x")
        );
        assert!(parse_tailnet(b"{}").is_err());
        assert!(cli_error(r#"{"error":{"code":"usage_error","message":"unrecognized subcommand 'discover'"},"exit_code":2}"#)
            .contains("newer phux CLI"));
    }
}
