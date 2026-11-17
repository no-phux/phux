//! `[forward]`: what `phux forward` may reach from this server, and
//! whether it brokers ssh-agent requests.

use serde::{Deserialize, Serialize};

/// `[forward]` table: TCP port and ssh-agent forwarding
/// (`docs/forwarding.md`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ForwardCfg {
    /// Serve `phux forward`. Off, the server does not advertise
    /// `FORWARD_TCP` and refuses the commands.
    pub tcp: bool,
    /// Targets a client may reach besides this machine's loopback, each a
    /// `host:port` or `ip:port` (`[::1]:port` for IPv6) matched exactly.
    /// The server dials these, so list only what every holder of input
    /// rights on it may reach.
    pub allow: Vec<String>,
    /// Broker ssh-agent requests from this server's panes to a client that
    /// runs `phux forward --agent`. On, every pane started afterwards gets
    /// `SSH_AUTH_SOCK` pointing at the server's broker socket, replacing
    /// any agent the server's own environment named. Off, the server does
    /// not advertise `FORWARD_AGENT`, refuses the commands, and leaves
    /// `SSH_AUTH_SOCK` alone.
    pub agent: bool,
}

impl Default for ForwardCfg {
    fn default() -> Self {
        Self {
            tcp: true,
            allow: Vec::new(),
            agent: false,
        }
    }
}
