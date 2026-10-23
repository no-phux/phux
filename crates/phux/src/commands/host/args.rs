//! The `phux host` subcommand and `host add` flag definitions.

use std::path::PathBuf;

use usage::{Args, Subcommands};

use super::super::JsonOpt;
use super::super::enroll::{self};

use super::HostRole;

/// The flags `phux host add` takes, shared with its hidden `enroll`
/// spelling.
#[derive(Debug, Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a usage flag struct; each bool is one independent CLI switch"
)]
pub(crate) struct AddOpts {
    /// Which registry the machine lands in: a server you attach to
    /// (`remote`, the default) or a peer this hub dials for its users.
    #[usage(long, value_enum, default = "remote")]
    pub(crate) role: HostRole,

    /// Local label to register. Defaults to HOST without any `user@`, or
    /// to the host of an endpoint URI.
    #[usage(long, value_name = "NAME")]
    pub(crate) name: Option<String>,

    /// Register this address instead of the routes detected on the host:
    /// `HOST:PORT` (dialed over QUIC) or a full `quic://`/`wss://` URI.
    #[usage(long = "endpoint", value_name = "HOST:PORT")]
    pub(crate) endpoint_override: Option<String>,

    /// QUIC port to configure on the host and dial.
    #[usage(long, value_name = "PORT", default = "8788", default_value_t = enroll::default_quic_port())]
    pub(crate) quic_port: u16,

    /// Start the host's server but skip installing its service unit. The
    /// server will not come back on its own after a reboot.
    #[usage(long)]
    pub(crate) no_service: bool,

    /// Register an `ssh://HOST` entry without contacting the host at all.
    #[usage(long, conflicts("--endpoint", "--no-service"))]
    pub(crate) ssh_only: bool,

    /// The `phux` to run on the host, for when a non-interactive ssh
    /// shell's `PATH` does not find it (a Homebrew or Nix install). It is
    /// recorded in the entry, so later attaches and renewals use it too;
    /// omitted, a re-add keeps the one recorded.
    #[usage(long, value_name = "PATH", default = "phux")]
    pub(crate) remote_phux: String,

    /// Install phux on the host with the official installer when it is
    /// missing, or update it with its own `phux update` when it is older
    /// than this one, on this machine's release channel. The installer puts
    /// it in the host's `~/.local/bin`; an update keeps the host's panes.
    #[usage(long, conflicts("--ssh-only", "--remote-phux"))]
    pub(crate) install: bool,

    /// Let ssh record the host key of a machine it has never connected to
    /// (`StrictHostKeyChecking=accept-new`) instead of refusing it; a key
    /// that changed is still refused. For a host whose address something
    /// else vouches for, such as a Tailscale peer.
    #[usage(long, conflicts("--ssh-only"))]
    pub(crate) accept_new_host_key: bool,

    /// Manual form only: absolute path to a file holding the pairing token
    /// minted by `phux pair` on the other machine.
    #[usage(long, value_name = "PATH")]
    pub(crate) token_file: Option<PathBuf>,

    /// Manual form only: the other machine's TLS certificate SHA-256
    /// fingerprint, as printed by `phux pair`. Required for `quic://` and
    /// `wss://`.
    #[usage(long, value_name = "FP")]
    pub(crate) cert_fingerprint: Option<String>,

    /// Manual form, `--role remote` only: the TLS server name (SNI) every
    /// dial offers instead of the endpoint's host. Through a relay this is
    /// the route: register the relay's `quic://RELAY:PORT` with the relay's
    /// `--cert-fingerprint`, the server's `--token-file`, and the server's
    /// `--ca-fingerprint`.
    #[usage(long, value_name = "NAME")]
    pub(crate) tls_server_name: Option<String>,

    /// Manual form with `--tls-server-name` on a `quic://` endpoint, and
    /// required there: the certificate authority of the server behind the
    /// relay route (`sha256:` and 64 hex digits, the `ca_fingerprint` `phux
    /// pair --relay-route` prints). Every dial runs end to end with that
    /// server inside the relayed stream, so the relay sees only ciphertext.
    #[usage(long, value_name = "SHA256")]
    pub(crate) ca_fingerprint: Option<String>,

    /// Session to attach on arrival (`--role remote` only). Omitted: the
    /// remote server's own last-attach memory decides.
    #[usage(long, value_name = "NAME")]
    pub(crate) session: Option<String>,

    /// Register the entry but leave it disabled (`--role satellite` only).
    #[usage(long)]
    pub(crate) disabled: bool,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}

/// `phux host <action>` — CRUD over both machine registries through one
/// namespace.
#[derive(Debug, Subcommands)]
pub(crate) enum HostAction {
    /// Add a machine so `phux attach NAME` reaches it.
    ///
    /// `phux host add me@mini` is all a new machine needs. Over the ssh
    /// trust you already have it confirms phux is installed there, starts
    /// the server and keeps it running across reboots, pairs, tries the
    /// direct routes, and registers the first one that answers. Nothing to
    /// type by hand: no port, no token, no fingerprint. Run it again on a
    /// registered machine to check it and repair it if it stopped
    /// answering.
    ///
    /// The manual form registers an endpoint you already hold credentials
    /// for: `phux host add NAME quic://HOST:PORT --token-file PATH
    /// --cert-fingerprint FP` (`wss://` likewise; `ssh://HOST` needs no
    /// credentials). It replaces the whole entry, so repeat the credential
    /// flags when re-adding a name. `--role satellite` registers a peer
    /// this hub dials for its users instead of a server you attach to.
    Add {
        /// `[USER@]HOST[:PORT]` to set up over ssh, or an endpoint URI
        /// (`quic://`, `wss://`, `ssh://`) to register as is.
        target: String,

        /// Manual form: the endpoint URI to register, with TARGET as the
        /// entry's name.
        endpoint: Option<String>,

        #[usage(flatten)]
        opts: AddOpts,
    },

    /// List registered machines from both registries.
    ///
    /// With no `--role`, remotes and satellites are merged into one table
    /// with a ROLE column; `--role` filters to one registry.
    #[usage(name = "ls", alias = "list")]
    List {
        /// Show only this registry.
        #[usage(long, value_enum)]
        role: Option<HostRole>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// List the machines on this computer's Tailscale network.
    ///
    /// Reads the local Tailscale client (`tailscale status --json`): whether
    /// it is signed in and connected, every other machine on the tailnet,
    /// which run an OS phux ships for, and which are already registered. Set
    /// one up with `phux host add DNS_NAME --name NAME --install`. Nothing is
    /// registered or contacted. `--login` first brings the client up: it opens
    /// the macOS app, reconnects a stopped client, or starts a browser sign-in
    /// and prints its link (`tailscale.auth_url` under `--json`), returning
    /// once the link is known while the sign-in finishes in the browser.
    Discover {
        /// Sign in to Tailscale, or reconnect it, before listing.
        #[usage(long)]
        login: bool,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Remove a registered machine. Its token file is left in place.
    ///
    /// With no `--role`, the name is resolved across both registries; a name
    /// registered in both is refused until `--role` disambiguates.
    #[usage(name = "rm", alias = "remove")]
    Remove {
        /// Registered name.
        name: String,

        /// Which registry to remove from. Omitted: both are searched.
        #[usage(long, value_enum)]
        role: Option<HostRole>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Show a registered machine, including its route and credential references.
    Show {
        /// Registered name.
        name: String,
        /// Disambiguate a name present in both registries.
        #[usage(long, value_enum)]
        role: Option<HostRole>,
        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Rename a registered machine without changing its route or credentials.
    Rename {
        /// Current registered name.
        name: String,
        /// New local label (does not rename the machine over the network).
        new_name: String,
        #[usage(long, value_enum)]
        role: Option<HostRole>,
        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Renew a remote's workload client certificate over ssh.
    ///
    /// Enrolls a fresh certificate the way `phux host add` does (the key is
    /// generated here; only its CSR crosses ssh), records it in the entry,
    /// and only then revokes the one it replaces on the far host. Nothing
    /// else about the entry changes: no re-pairing, no service install.
    /// Certificates are issued for 90 days; dials warn and `phux doctor`
    /// reports one within 14 days of expiry.
    Renew {
        /// Registered remote name.
        name: String,

        /// The `phux` to run on the host, for when a non-interactive ssh
        /// shell's `PATH` does not find it. Omitted: the one the entry
        /// recorded when it was added.
        #[usage(long, value_name = "PATH", default = "phux")]
        remote_phux: String,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Move registered remotes from `wss://` (or ssh) onto QUIC.
    ///
    /// For each remote, or just NAME, dials its QUIC route with the saved
    /// token and pin: a kept direct route, else (beside the default
    /// `wss://HOST:8787`, not on loopback) the same host on port 8788, where
    /// the server binds QUIC beside WebSocket with the same certificate and
    /// tokens. When it answers, the entry is rewritten to
    /// `quic://`; otherwise it is left as it was. An attach does the same
    /// automatically, with a shorter wait; this verb is the explicit,
    /// reported form. `phux host ls` shows each entry's transport.
    Upgrade {
        /// Registered remote name. Omitted: every remote.
        name: Option<String>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Attach to a registered remote host (same as `phux attach NAME`).
    Attach {
        /// Registered remote name.
        name: String,
    },

    /// Enable a satellite; a running local hub dials it now.
    Enable {
        /// Registered satellite name.
        name: String,
        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Disable a satellite without forgetting it; a running hub drops its link.
    Disable {
        /// Registered satellite name.
        name: String,
        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Mint a one-time code a machine this hub cannot reach joins with.
    ///
    /// For a sandbox or VM behind NAT, with no listener the hub could dial.
    /// Registers NAME as an `inbound://` satellite and prints a join code
    /// carrying this hub's QUIC address, the CA it is pinned by, and a
    /// single-use ticket. On that machine, `phux host join CODE` enrolls a
    /// key it keeps and dials out to this hub, which adopts the connection
    /// as NAME's link. The hub must run `[policy] mode = "paired"`. Revoke an
    /// unused code with `phux workload revoke t-…`, and the joined machine
    /// with `phux host rm NAME`.
    Ticket {
        /// Satellite name: how this hub addresses the machine's terminals.
        name: String,

        /// `HOST:PORT` the satellite dials. Omitted: this server's live QUIC
        /// listener, as `phux pair` advertises it.
        #[usage(long, value_name = "HOST:PORT")]
        hub: Option<String>,

        /// Minutes the code stays redeemable.
        #[usage(
            long,
            default = "10",
            default_value_t = 10,
            validate = "int(value) >= 1 && int(value) <= 1440",
            validate_error = "must be between 1 and 1440 minutes",
            value_name = "MINUTES"
        )]
        expires_in: i64,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Join a hub as a satellite with the code `phux host ticket` printed.
    ///
    /// Generates a key here (it never leaves this machine), enrolls it with
    /// the code's single-use ticket, records an `[[uplink]]` entry, and asks
    /// the running server to dial the hub. The server keeps that connection
    /// up, redialing after any drop, and needs no listener of its own: the
    /// hub reaches this machine's terminals through it.
    Join {
        /// The `phux://satellite?...` code from the hub, or `-` to read it
        /// from stdin and keep it out of `ps` and shell history.
        code: String,

        #[usage(flatten)]
        json: JsonOpt,
    },
}
