//! The `Machines` verbs that pair, enroll, and reach other machines.

use crate::commands::{RemoteOpt, host, pair::PairAction, workload};
use usage::Args;

/// Download a file from a server or one of its satellites
#[derive(Debug, Args)]
#[usage(
    long_about = "Download a file from the server you connect to, or from one of its satellites, to this client's filesystem.

SOURCE is an absolute path on that server (`/Users/me/deck.pptx`) or `HOST:/path` for a satellite the server federates; any regular file the serving user can read. LOCAL defaults to SOURCE's file name in the current directory, and a directory LOCAL receives the file under its name. An existing LOCAL is refused unless --force replaces it. Bytes land in a fresh sibling `.phux-part` file and move into place only after the whole-file SHA-256 verifies, so a refusal leaves nothing behind.

Download dials configured remote transports headlessly; it never pairs over SSH. The timeout covers resolution and transfer. Completed prints LOCAL; --json prints the receipt for completed or refused outcomes. Refused exits nonzero."
)]
pub(crate) struct DownloadArgs {
    /// Absolute path on the server, or `HOST:/path` on a satellite.
    pub(crate) source: String,
    /// Destination file or directory on this client's filesystem.
    pub(crate) local: Option<std::path::PathBuf>,
    /// Replace an existing LOCAL.
    #[usage(long)]
    pub(crate) force: bool,
    /// Overall deadline in seconds, bounded to 1..=3600.
    #[usage(long, default = "60", value_name = "SECS")]
    pub(crate) timeout: u64,
    /// Print the structured receipt on stdout, including refusals.
    #[usage(long)]
    pub(crate) json: bool,
    #[usage(flatten)]
    pub(crate) remote: RemoteOpt,
}

/// Mint, rotate, revoke, list, or prune remote credentials
///
/// With no subcommand, mint one credential into the server's store and
/// print its stable ID, one-time bearer secret, and certificate fingerprint.
/// On a terminal the connect link is followed by a scannable QR of it
/// (`--no-qr` leaves it out). Both forms include phone enrollment when a
/// QR is shown; they refuse before minting if the phone cannot enroll.
/// `--qr` is the camera form: a large terminal QR and a clear expiry line,
/// with the bearer and connect link omitted from the text (they exist only
/// inside the QR). `--secret-file PATH` is the off-camera pre-pair: the
/// bearer is written to a new mode-0600 file and is not printed. `--json`
/// still prints the bearer for `phux host add` and cannot combine with
/// `--qr` or `--secret-file`. `ls` lists ids with mint time, last seen,
/// and revoked status; `prune --unused-for DURATION` revokes idle credentials; `rotate`
/// replaces the bearer with a bounded overlap; `revoke` denies all
/// generations on future connections. These operations update the store
/// directly and take effect without restarting the server.
///
/// Minting first asks the running server (`--socket`) which remote
/// listeners it has bound, and mints nothing when none would accept the
/// credential: no server, no bound listener, `--host` with no wss
/// listener, or `--qr` with no address a device can dial. On a tailnet
/// with `MagicDNS` the link names the host's `MagicDNS` name rather than its
/// tailnet address. `--relay-route`
/// mints for a relay route instead, which needs the server to answer but
/// no bound listener. `ls`, `prune`, `rotate`, and `revoke` only edit the
/// store and need no server.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent CLI flag per bool, as the verb declares them"
)]
#[derive(Debug, Args)]
pub(crate) struct PairArgs {
    #[usage(subcommand)]
    pub(crate) action: Option<PairAction>,

    /// Versioned credential store to update. Defaults to `PHUX_WS_TOKENS`.
    #[usage(long, global, value_name = "PATH")]
    pub(crate) tokens: Option<std::path::PathBuf>,

    /// Server certificate PEM, used to print the pairing fingerprint.
    /// Defaults to `PHUX_WS_TLS_CERT`.
    #[usage(long, value_name = "PATH")]
    pub(crate) cert: Option<std::path::PathBuf>,

    /// Render the pairing link as a camera-sized terminal QR and do not
    /// print the bearer or the link. The QR is the largest scale that
    /// fits the terminal width. The expiry line says when the credential
    /// stops working, or that it does not expire. Needs a server address:
    /// pass `--host`, or let it derive one from the running server's
    /// bound wss listener. Also includes a single-use phone enrollment
    /// ticket (as `--enroll` does): needs a device-dialable QUIC listener
    /// and a server certificate issued by its workload CA. Refused before
    /// minting if any of these are missing. Cannot combine with `--json`,
    /// which prints the bearer.
    #[usage(long, conflicts("--json", "--no-qr"))]
    pub(crate) qr: bool,

    /// Write the bearer and connect link to PATH (mode 0600, created
    /// anew) and do not print them. PATH must not already exist. This is
    /// the off-camera pre-pair: the filmed terminal never shows the
    /// token. Cannot combine with `--json`, which prints the bearer.
    #[usage(long, value_name = "PATH", conflicts("--json"))]
    pub(crate) secret_file: Option<std::path::PathBuf>,

    /// Absolute expiry for the minted credential (`30d`, `24h`, `15m`,
    /// `45s`, or a bare number of seconds), from 1 second through 30
    /// days. Omitted: the credential does not expire. The human output
    /// always states which of those is true. Admission fails after the
    /// instant; revocation still works.
    #[usage(long, value_name = "DURATION")]
    pub(crate) expires: Option<String>,

    /// Do not render the QR a terminal mint prints beneath the connect
    /// link by default. The link is still printed as text.
    #[usage(long)]
    pub(crate) no_qr: bool,

    /// Server address (`host:port`, or a full `ws://`/`wss://` URL) to
    /// embed in the connect link so it is fully self-contained; the
    /// running server must have a wss listener bound behind it. Omitted:
    /// the address its wss listener is bound to (an overlay address for
    /// a `0.0.0.0`/`::` bind); with no such address no link is printed
    /// (the device enters the address itself).
    #[usage(long, value_name = "HOST:PORT")]
    pub(crate) host: Option<String>,

    /// Human-readable server name to embed in the connect link, shown by
    /// the device in its server list. Omitted: the device picks a default.
    #[usage(long, value_name = "NAME")]
    pub(crate) name: Option<String>,

    /// Emit the mint, rotation, revocation, list, or prune result as JSON
    /// on stdout. `phux host add` consumes the mint document over ssh.
    #[usage(long, global)]
    pub(crate) json: bool,

    /// When minting, revoke any live credential whose bearer matches this
    /// hex token first. `phux host add` passes the previously enrolled
    /// token so re-enrollment does not leave abandoned live credentials.
    #[usage(long, value_name = "HEX")]
    pub(crate) replace_token: Option<String>,

    /// Mint for a relay route instead of a listener: the connect link
    /// dials the `[[connector]]` relay this server tunnels to, pins the
    /// relay's certificate, and names ROUTE as its TLS server name (the
    /// route `phux relay pair --route` enrolled). Needs a running server
    /// but no bound listener; the link reaches the server while its
    /// connector holds the route.
    #[usage(long, value_name = "ROUTE", conflicts("--host"))]
    pub(crate) relay_route: Option<String>,

    /// The `[[connector]]` relay (its `relay = "HOST:PORT"`) a
    /// `--relay-route` link dials. Needed only when several are
    /// configured.
    #[usage(long, value_name = "HOST:PORT", requires("--relay-route"))]
    pub(crate) relay: Option<String>,

    /// Also mint a single-use enrollment ticket into the link, so a
    /// device without ssh (a phone) enrolls a workload certificate for a
    /// key it generates and keeps. Needed to reach a server in `paired`
    /// policy mode. A QR shown on a terminal implies this, including
    /// through a relay. The ticket expires in ten minutes. With
    /// `--relay-route` the device enrolls end to end through the relay.
    #[usage(long)]
    pub(crate) enroll: bool,
}

/// Manage the mTLS workload authority
///
/// The workload CA and the registry of client credentials it admits over
/// mutual TLS. `authority` prints the CA fingerprint (`--init` creates
/// the CA); `add-key` enrolls a client certificate or signs a CSR read
/// from stdin or `--file`, never from the command line; `list` and
/// `revoke` show and retire credentials. These write the state directory
/// directly and never contact a server; a running server applies each
/// change on its next connection, with no restart.
#[derive(Debug, Args)]
pub(crate) struct WorkloadArgs {
    #[usage(subcommand)]
    pub(crate) action: workload::WorkloadAction,

    /// Emit the result as JSON on stdout.
    #[usage(long, global)]
    pub(crate) json: bool,
}

/// Add and manage the machines phux reaches
///
/// One namespace over both machine registries. `--role remote` (the
/// default) manages the servers `phux attach <name>` dials; `--role
/// satellite` manages the peers a federation hub dials for its users.
/// The two registries stay separate in config (`[[remote]]` vs
/// `[[satellites]]`) because they encode opposite trust directions;
/// this verb absorbs the split into a flag.
#[derive(Debug, Args)]
pub(crate) struct HostArgs {
    #[usage(subcommand)]
    pub(crate) action: host::HostAction,
}
