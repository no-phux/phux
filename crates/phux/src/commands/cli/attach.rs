//! The `Sessions` verbs that attach to and detach from a session.

use crate::commands::{JsonOpt, RecOpts, RemoteOpt};
use usage::Args;

/// Attach to a session, here or on a registered host
///
/// Interactive: requires a TTY. With no name, attaches to the
/// most-recently-focused session, auto-spawning a server if none is
/// running.
///
/// Attach never creates a session on a running server: an unknown name
/// is an error (`phux new NAME` creates one). With no server running, the
/// auto-spawned server seeds its first session under NAME.
///
/// A name registered as a host (`phux host add`) shadows a local session of the same name: `phux attach
/// NAME` dials the registered host instead of the local socket.
/// Pass `--socket` to force the local reading of the name.
#[derive(Debug, Args)]
pub(crate) struct AttachArgs {
    /// Session name (matches the name used at creation time).
    ///
    /// Omit to attach to the most-recently-focused session.
    pub(crate) session: Option<String>,

    /// Attach over QUIC to a remote `phux server --quic` listener at this
    /// `HOST:PORT` instead of the local Unix socket. HOST may be an IP
    /// literal or a DNS name (e.g. a Tailscale `MagicDNS` name), resolved
    /// before dialing. QUIC is always TLS 1.3-encrypted. A target
    /// resolving to loopback trusts the server's self-signed cert for
    /// local dev; any routable address requires `--cert-fingerprint`
    /// (the value `phux pair` prints on the server host).
    #[usage(
        long,
        value_name = "HOST:PORT",
        group = "remote_transport",
        help_heading = "Direct dial"
    )]
    pub(crate) quic: Option<String>,

    /// Attach over WebSocket to a `phux server --listen` endpoint. Use
    /// `ws://HOST:PORT` for loopback dev, or `wss://HOST:PORT` with
    /// `--token` and `--cert-fingerprint` for routable remote attach. This
    /// is the TCP fallback when UDP/QUIC is blocked.
    #[usage(
        long,
        value_name = "URL",
        group = "remote_transport",
        help_heading = "Direct dial"
    )]
    pub(crate) ws: Option<String>,

    /// Bearer pairing token (hex) for an authenticated QUIC listener, as
    /// minted by `phux pair`. QUIC sends it as the stream's opening
    /// preamble; WebSocket sends it as `Authorization: Bearer`.
    /// Requires `--quic` or `--ws`.
    // No parser-level `requires("--quic", "--ws")`: usage-rs treats that
    // as all-of, which rejected a single transport. The any-of rule is
    // enforced in `run_attach` (exit 2), matching `phux logs -f`.
    #[usage(long, help_heading = "Direct dial")]
    pub(crate) token: Option<String>,

    /// Pin the QUIC server's certificate by its SHA-256 fingerprint (the
    /// value `phux pair` prints). Required to dial any non-loopback
    /// `--quic`/`--ws wss://` address. Requires `--quic` or `--ws`.
    #[usage(long, value_name = "FP", help_heading = "Direct dial")]
    pub(crate) cert_fingerprint: Option<String>,

    /// TLS server name (SNI) to offer the remote listener. With `--quic`
    /// it names a relay route: `--quic` is the relay, `--cert-fingerprint`
    /// its pin, and `--ca-fingerprint` is required. WebSocket defaults
    /// to the URL host. Requires `--quic` or `--ws`.
    #[usage(long, value_name = "NAME", help_heading = "Direct dial")]
    pub(crate) tls_server_name: Option<String>,

    /// The certificate authority of the server behind a relay route
    /// (`sha256:` and 64 hex digits, the `ca_fingerprint` `phux pair
    /// --relay-route` prints). The session runs end to end with that
    /// server inside the relayed stream, so the relay sees only
    /// ciphertext. Requires `--quic` and `--tls-server-name`.
    #[usage(long, value_name = "SHA256", help_heading = "Direct dial")]
    pub(crate) ca_fingerprint: Option<String>,

    /// Attach to a phux server on another machine, ssh-style:
    /// `--remote me@mini`. Resolves to a registered host when there is
    /// one, otherwise pairs the host first (over a `--code`, or over
    /// your existing ssh trust) and registers the result, so every
    /// later attach is a direct QUIC dial with no ssh in the path.
    ///
    /// PORT defaults to 8788, the port a server auto-binds on its
    /// overlay address. The `user@` half names the ssh destination used
    /// to pair; it is not sent on the wire.
    #[usage(
        long,
        value_name = "[USER@]HOST[:PORT]",
        conflicts("--quic", "--ws", "--ssh"),
        help_heading = "Remote host"
    )]
    pub(crate) remote: Option<String>,

    /// Pair `--remote` from a `https://phux.sh/connect?...` link
    /// (or its `phux://connect?...` spelling) instead of over ssh — the
    /// same link `phux pair` prints and `phux pair --qr` renders. Quote
    /// it: it contains `&`.
    #[usage(
        long,
        value_name = "LINK",
        requires("--remote"),
        help_heading = "Remote host"
    )]
    pub(crate) code: Option<String>,

    /// Never shell out to ssh for `--remote`. An unregistered host is
    /// refused with its remedies named instead of paired.
    #[usage(long, requires("--remote"), help_heading = "Remote host")]
    pub(crate) no_enroll: bool,

    /// Attach mosh-style over ssh: run `phux bootstrap` on the host
    /// through ssh, which starts the server there if needed and opens a
    /// QUIC listener for this attach alone, then dial it directly. ssh
    /// authenticates you (password and 2FA prompts work) and exits once
    /// the session is up; the session itself rides QUIC, so it roams and
    /// renders locally. Needs no pairing, service, or overlay network on
    /// the host. Falls back to `ssh -t HOST phux attach` when UDP cannot
    /// reach it. The host is anything ssh accepts, including
    /// `ssh://user@host:port` and aliases from `~/.ssh/config`.
    #[usage(
        long,
        value_name = "[USER@]HOST",
        conflicts("--quic", "--ws", "--remote"),
        help_heading = "Remote host"
    )]
    pub(crate) ssh: Option<String>,

    /// The `phux` to run on the `--ssh` host, for when a non-interactive
    /// ssh shell's `PATH` does not find it (a Homebrew or Nix install).
    #[usage(
        long,
        value_name = "PATH",
        requires("--ssh"),
        default = "phux",
        help_heading = "Remote host"
    )]
    pub(crate) remote_phux: String,

    /// Bind the `--ssh` host's listener to a UDP port in this inclusive
    /// range, e.g. `60000-61000`, so one firewall rule covers every
    /// attach. Any free port by default.
    #[usage(
        long,
        value_name = "MIN-MAX",
        requires("--ssh"),
        help_heading = "Remote host"
    )]
    pub(crate) udp_ports: Option<String>,

    // ADR-0127: a declared VIEWER role on every pane this attach opens.
    /// Attach as a viewer: watch every pane, type into none.
    /// The server refuses this attach's input, and widening it takes a
    /// fresh attach without the flag, which every watcher sees. Your
    /// viewport still sizes the panes.
    #[usage(long, conflicts("--take"), help_heading = "Attach role")]
    pub(crate) viewer: bool,

    // ADR-0127: PRIMARY with DELIBERATE takeover, spent on this attach.
    /// Attach and take the wheel: seize the input lease of
    /// every pane this attach opens, in the same step as the attach. The
    /// previous holder stays attached. `phux give` hands it back.
    #[usage(long, conflicts("--viewer"), help_heading = "Attach role")]
    pub(crate) take: bool,

    /// Tee this attach's composited output to a recording. Declared here
    /// (and on the root command) rather than globally so it only shows up
    /// on the verbs that raise a TUI.
    #[usage(flatten, next_help_heading = "Recording")]
    pub(crate) rec: RecOpts,
}

/// Detach clients from a session
///
/// The CLI counterpart to the `C-a d` keybinding. With `SESSION`, detaches
/// every client attached to that session; with no argument, detaches every
/// attached client on the server. Each target client's TUI exits cleanly.
/// Useful for scripting or reclaiming a session that's attached (or wedged)
/// elsewhere.
#[derive(Debug, Args)]
pub(crate) struct DetachArgs {
    /// Session to detach clients from. Omit to detach every attached
    /// client on the server.
    pub(crate) session: Option<String>,

    /// Detach without asking.
    ///
    /// Without it, `phux detach` asks on a terminal, and when stdin is
    /// not one it refuses with exit 2 having sent nothing.
    #[usage(long)]
    pub(crate) yes: bool,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,

    #[usage(flatten)]
    pub(crate) remote: RemoteOpt,
}
