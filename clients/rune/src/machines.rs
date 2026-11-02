//! The machines this computer reaches, through the installed CLI: its
//! registry, its Tailscale discovery, and `phux host add`, which owns
//! enrollment and trust. This only reads the answers.

use std::io::BufRead as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use crate::launch::run_for;

const LIST_TIMEOUT: Duration = Duration::from_secs(15);

/// What the sidebar calls the server on this computer.
pub const LOCAL: &str = if cfg!(target_os = "macos") {
    "This Mac"
} else {
    "This computer"
};

/// A machine in this computer's registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub name: String,
    /// The ssh destination it was set up over.
    pub ssh: Option<String>,
    /// Its only route is ssh, which needs a terminal: a window can't dial it.
    pub needs_direct: bool,
}

/// A tailnet machine phux can run on that isn't registered yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    /// The `MagicDNS` name `phux host add` is given.
    pub ssh_host: String,
    pub online: bool,
}

/// What `phux host add` is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Add {
    pub target: String,
    pub name: Option<String>,
    /// The tailnet vouches for the address, so a first host key is accepted.
    pub tailnet: bool,
}

impl Add {
    fn args(&self) -> Vec<String> {
        let mut args = vec![
            "host".to_owned(),
            "add".to_owned(),
            self.target.clone(),
            "--install".to_owned(),
        ];
        if let Some(name) = &self.name {
            args.extend(["--name".to_owned(), name.clone()]);
        }
        if self.tailnet {
            args.push("--accept-new-host-key".to_owned());
        }
        args
    }
}

fn parse_hosts(value: &Value) -> Vec<Host> {
    let hosts = value["hosts"].as_array().into_iter().flatten();
    hosts
        .filter(|host| host["role"] == "remote")
        .filter_map(|host| {
            Some(Host {
                name: host["name"].as_str()?.to_owned(),
                ssh: host["ssh"].as_str().map(str::to_owned),
                needs_direct: host["endpoint"]
                    .as_str()
                    .is_some_and(|endpoint| endpoint.trim().starts_with("ssh://")),
            })
        })
        .collect()
}

/// The peer an ssh destination names by its full `MagicDNS` name. A short
/// name isn't enough: ssh may resolve it through its config or the LAN to a
/// machine the tailnet doesn't vouch for.
pub fn peer_for<'a>(peers: &'a [Peer], target: &str) -> Option<&'a Peer> {
    let target = target.trim();
    let host = target.rsplit_once('@').map_or(target, |(_, host)| host);
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    peers.iter().find(|peer| peer.ssh_host == host)
}

/// The registered host a finished add made or re-enrolled: the name it was
/// asked for, a new row, else the row with its ssh destination.
pub fn added<'a>(hosts: &'a [Host], before: &[Host], request: &Add) -> Option<&'a Host> {
    let named = |name: &str| hosts.iter().find(|host| host.name == name);
    let new = || hosts.iter().find(|host| !before.contains(host));
    let same_route = || {
        let target = request.target.trim();
        let bare = target.rsplit_once('@').map_or(target, |(_, host)| host);
        hosts
            .iter()
            .find(|host| host.ssh.as_deref() == Some(target) || host.name == bare)
    };
    request
        .name
        .as_deref()
        .and_then(named)
        .or_else(new)
        .or_else(same_route)
}

fn parse_peers(value: &Value) -> Vec<Peer> {
    let peers = value["peers"].as_array().into_iter().flatten();
    peers
        .filter(|peer| peer["supported"] == true && peer["registered"].is_null())
        .filter_map(|peer| {
            Some(Peer {
                name: peer["name"].as_str()?.to_owned(),
                ssh_host: peer["ssh_host"].as_str()?.to_owned(),
                online: peer["online"] == true,
            })
        })
        .collect()
}

fn json(phux: &Path, args: &[&str]) -> Result<Value, String> {
    let output = run_for(phux, args, LIST_TIMEOUT)?;
    serde_json::from_slice(&output).map_err(|error| format!("phux printed no JSON: {error}"))
}

pub fn registered(phux: &Path) -> Result<Vec<Host>, String> {
    Ok(parse_hosts(&json(phux, &["host", "ls", "--json"])?))
}

/// Empty without Tailscale: discovery is a convenience, typing still works.
pub fn tailnet(phux: &Path) -> Vec<Peer> {
    json(phux, &["host", "discover", "--json"]).map_or_else(|_| Vec::new(), |doc| parse_peers(&doc))
}

/// Take `name` out of the registry. Its sessions keep running over there.
pub fn remove(phux: &Path, name: &str) -> Result<(), String> {
    run_for(
        phux,
        &["host", "rm", "--role", "remote", name],
        LIST_TIMEOUT,
    )
    .map(|_| ())
}

/// `phux: info: mini: trying quic://...` reads `trying quic://...`.
fn step(line: &str) -> Option<String> {
    let (severity, text) = line.strip_prefix("phux: ")?.split_once(": ")?;
    if !matches!(severity, "info" | "ok" | "warn") {
        return None;
    }
    let text = text
        .split_once(": ")
        .filter(|(name, _)| !name.contains(' '))
        .map_or(text, |(_, step)| step);
    Some(text.trim().to_owned()).filter(|step| !step.is_empty())
}

fn failure(lines: &[String]) -> String {
    let from = lines
        .iter()
        .position(|line| line.starts_with("phux: error") || line.starts_with("error"));
    let useful = from.map_or(lines, |from| &lines[from..]);
    useful.join("\n")
}

/// Enrollment can install phux and pair, so it takes a while; each step goes
/// to `report` as the CLI prints it. stdin stays closed, so a machine that
/// wants a password fails with ssh's words instead of hanging.
pub fn add(phux: &Path, request: &Add, report: &dyn Fn(String)) -> Result<(), String> {
    let mut child = Command::new(phux)
        .args(request.args())
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run {}: {error}", phux.display()))?;
    let mut lines = Vec::new();
    if let Some(stderr) = child.stderr.take() {
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(step) = step(&line) {
                report(step);
            }
            lines.push(line);
        }
    }
    let status = child
        .wait()
        .map_err(|error| format!("waiting for {}: {error}", phux.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(failure(&lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_remotes_are_machines_to_attach() {
        let doc = json!({ "schema_version": 1, "hosts": [
            { "name": "mini", "role": "remote" },
            { "name": "box", "role": "satellite" },
        ]});
        let hosts = parse_hosts(&doc);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "mini");
    }

    #[test]
    fn an_ssh_route_is_marked() {
        let doc = json!({ "hosts": [
            { "name": "a", "role": "remote", "endpoint": "quic://a:8788", "ssh": "me@a" },
            { "name": "b", "role": "remote", "endpoint": "ssh://me@b" },
        ]});
        let hosts = parse_hosts(&doc);
        assert!(!hosts[0].needs_direct);
        assert_eq!(hosts[0].ssh.as_deref(), Some("me@a"));
        assert!(hosts[1].needs_direct);
    }

    fn peer(ssh_host: &str) -> Peer {
        Peer {
            name: "mini".to_owned(),
            ssh_host: ssh_host.to_owned(),
            online: true,
        }
    }

    #[test]
    fn a_typed_name_vouches_only_with_its_full_tailnet_name() {
        let peers = [peer("mini.tail.ts.net")];
        for target in ["me@mini.tail.ts.net", " MINI.tail.ts.net. "] {
            assert!(peer_for(&peers, target).is_some(), "{target}");
        }
        for target in ["mini", "me@mini", "other.tail.ts.net"] {
            assert!(peer_for(&peers, target).is_none(), "{target}");
        }
    }

    fn host(name: &str, ssh: Option<&str>) -> Host {
        Host {
            name: name.to_owned(),
            ssh: ssh.map(str::to_owned),
            needs_direct: false,
        }
    }

    #[test]
    fn an_add_lands_on_what_it_registered() {
        let request = |target: &str, name: Option<&str>| Add {
            target: target.to_owned(),
            name: name.map(str::to_owned),
            tailnet: false,
        };
        let before = [host("a", Some("me@a"))];
        let new = [host("a", Some("me@a")), host("b", Some("me@b"))];
        let found =
            |hosts: &[Host], request: Add| added(hosts, &before, &request).map(|h| h.name.clone());
        assert_eq!(found(&new, request("me@b", None)).as_deref(), Some("b"));
        // Setting a known machine up again adds no row.
        assert_eq!(found(&before, request("me@a", None)).as_deref(), Some("a"));
        assert_eq!(found(&before, request("a", None)).as_deref(), Some("a"));
        assert_eq!(found(&new, request("x", Some("a"))).as_deref(), Some("a"));
        assert_eq!(found(&before, request("me@z", None)), None);
    }

    #[test]
    fn peers_are_supported_and_unregistered() {
        let peer = |name: &str, supported: bool, registered: Value| {
            json!({ "name": name, "ssh_host": format!("{name}.ts.net"), "online": true,
                    "supported": supported, "registered": registered })
        };
        let doc = json!({ "peers": [
            peer("mini", true, Value::Null),
            peer("phone", false, Value::Null),
            peer("old", true, json!("old")),
        ]});
        let peers = parse_peers(&doc);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].ssh_host, "mini.ts.net");
    }

    #[test]
    fn a_host_key_is_accepted_only_for_the_tailnet() {
        let add = |tailnet| Add {
            target: "me@mini".to_owned(),
            name: None,
            tailnet,
        };
        assert!(
            !add(false)
                .args()
                .contains(&"--accept-new-host-key".to_owned())
        );
        assert!(
            add(true)
                .args()
                .contains(&"--accept-new-host-key".to_owned())
        );
    }

    #[test]
    fn progress_drops_the_prefix_and_the_name() {
        assert_eq!(
            step("phux: info: mini: trying quic://x").as_deref(),
            Some("trying quic://x")
        );
        assert_eq!(step("phux: error: nope"), None);
        assert_eq!(step("noise"), None);
    }

    #[test]
    fn a_failure_starts_at_the_error() {
        let lines = ["phux: info: step", "phux: error: refused", "hint: try"].map(str::to_owned);
        assert_eq!(failure(&lines), "phux: error: refused\nhint: try");
    }

    #[test]
    fn add_reports_steps_and_fails_with_the_cli_words() {
        let dir = std::env::temp_dir().join(format!("phux-rune-add-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let phux = dir.join("phux");
        std::fs::write(
            &phux,
            "#!/bin/sh\necho 'phux: info: mini: dialing' >&2\necho 'phux: error: no route' >&2\nexit 1\n",
        )
        .unwrap();
        Command::new("/bin/chmod")
            .arg("+x")
            .arg(&phux)
            .status()
            .unwrap();
        let steps = std::sync::Mutex::new(Vec::new());
        let request = Add {
            target: "mini".to_owned(),
            name: None,
            tailnet: false,
        };
        let result = add(&phux, &request, &|step| steps.lock().unwrap().push(step));
        assert_eq!(result.unwrap_err(), "phux: error: no route");
        assert_eq!(*steps.lock().unwrap(), ["dialing"]);
    }
}
