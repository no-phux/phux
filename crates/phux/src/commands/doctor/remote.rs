//! Remote listeners: whether they are bound and reachable off this host.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use super::{Check, cli_runtime};

/// How long the reachability probe waits for the listener to answer.
pub(super) const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// What dialing our own routable listener revealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reachability {
    /// The listener answered (a handshake or an auth refusal both prove packets
    /// reached phux).
    Answered,
    /// The connection was refused, so nothing is bound there.
    NoListener,
    /// Accepted, then silence: the shape of a host packet filter.
    Silent,
    /// The address could not be reached at all.
    Unreachable,
}

/// Did every remote listener the running server expected actually bind? Asks
/// the server over UDS, so a fixed-on-disk store cannot hide a dead remote
/// surface.
pub(super) fn check_remote_listeners(socket_path: &std::path::Path) -> Check {
    if !socket_path.exists() {
        return Check::warn(
            "remote-listeners",
            "no server to ask about remote listeners",
            "start one with `phux` (auto-spawns) or `phux server`",
        );
    }
    let Ok(rt) = cli_runtime() else {
        return Check::warn(
            "remote-listeners",
            "could not build a runtime to ask the server about remote listeners",
            "retry; if this persists it is a bug worth filing",
        );
    };
    let report = match rt.block_on(phux_client::state::get_state(socket_path)) {
        Ok(view) => view.snapshot().listeners().cloned(),
        Err(err) => {
            let pid = crate::commands::diagnostics::recovery_pid(&err, socket_path);
            return remote_probe_failure(socket_path, &err, pid);
        }
    };
    let token_store_ok = phux_server::auth::ReloadingTokenStore::load(
        std::env::var_os("PHUX_WS_TOKENS")
            .map_or_else(phux_server::auth::default_token_store_path, PathBuf::from),
    )
    .is_ok();
    remote_listeners_check(report.as_ref(), token_store_ok)
}

/// The hint when the remote-listener probe cannot ask the server.
///
/// Same three-way split as the server check: a dead socket may be called
/// stale, a live incompatible server fails with the stop-and-start recovery,
/// and every other failure stays neutral.
pub(super) fn remote_probe_failure(
    socket_path: &std::path::Path,
    err: &phux_client::attach::AttachError,
    pid: Option<i32>,
) -> Check {
    use crate::commands::diagnostics::{
        STALE_SOCKET_HINT, SocketFailure, UNANSWERED_SOCKET_HINT, socket_failure,
    };
    let detail = format!("could not ask the server about remote listeners: {err}");
    match socket_failure(socket_path, err, pid) {
        SocketFailure::Incompatible(lines) => Check::fail(
            "remote-listeners",
            lines.first().cloned().unwrap_or_default(),
            lines.into_iter().skip(1).collect::<Vec<_>>().join("\n"),
        ),
        SocketFailure::Dead => Check::warn("remote-listeners", detail, STALE_SOCKET_HINT),
        SocketFailure::Other => Check::warn("remote-listeners", detail, UNANSWERED_SOCKET_HINT),
    }
}

/// The pure half of [`check_remote_listeners`].
pub(super) fn remote_listeners_check(
    report: Option<&phux_protocol::wire::RemoteListenersReport>,
    token_store_ok: bool,
) -> Check {
    let Some(report) = report else {
        return Check::pass(
            "remote-listeners",
            "server did not publish a remote-listener report (no remote transport configured, \
             or an older server)",
        );
    };
    if report.listeners.is_empty() {
        return Check::pass(
            "remote-listeners",
            "no remote listeners were configured or auto-bound",
        );
    }
    let unhealthy: Vec<_> = report.unhealthy().collect();
    if unhealthy.is_empty() {
        let bound = report
            .listeners
            .iter()
            .filter(|slot| slot.bound)
            .map(|slot| slot.transport.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Check::pass(
            "remote-listeners",
            format!("remote listeners bound ({bound})"),
        );
    }
    let detail = unhealthy
        .iter()
        .map(|slot| {
            let reason = slot.disabled_reason.map_or(
                "unknown",
                phux_protocol::wire::ListenerDisabledReason::as_str,
            );
            let addr = slot.addr.as_deref().unwrap_or("?");
            format!("{} at {addr} disabled ({reason})", slot.transport)
        })
        .collect::<Vec<_>>()
        .join("; ");
    Check::fail(
        "remote-listeners",
        format!("remote surface down: {detail}"),
        remote_listeners_hint(&unhealthy, token_store_ok),
    )
}

/// The remedy for the listeners `unhealthy` names, most specific cause first.
pub(super) fn remote_listeners_hint(
    unhealthy: &[&phux_protocol::wire::RemoteListenerSlot],
    token_store_ok: bool,
) -> &'static str {
    use phux_protocol::wire::ListenerDisabledReason;
    let any = |reason: ListenerDisabledReason| {
        unhealthy
            .iter()
            .any(|slot| slot.disabled_reason == Some(reason))
    };
    if any(ListenerDisabledReason::PolicyModeRefused) {
        return "the server resumed across an upgrade with no [policy] mode admitting a remote \
                listener, and kept its panes by leaving them unbound: set [policy] mode = \
                \"paired\" in config.toml (or remove the listener), then `phux upgrade` so they \
                bind with panes intact";
    }
    if any(ListenerDisabledReason::TokenStoreLoadFailed) {
        return if token_store_ok {
            "the credential store loads now, but this server disabled the listeners at boot — \
             restart or `phux upgrade` so they re-bind"
        } else {
            "fix the credential store (remove a pre-versioned file and pair again), \
             then restart or `phux upgrade` the server"
        };
    }
    "check the server log for the bind error, then restart or `phux upgrade` after fixing it"
}

/// Does traffic to this server's routable wss listener actually reach it?
/// Dials the bound off-loopback address with the real client stack, catching
/// a firewall stealth-drop that every local check reports as healthy. A
/// loopback-only bind is skipped; `0.0.0.0`/`::` is rewritten onto a detected
/// overlay IP, otherwise warned.
pub(super) fn check_remote_reachable(socket_path: &std::path::Path) -> Check {
    match server_wss_offer(socket_path) {
        WssOffer::Disabled(reason) => remote_reachable_check(
            "this server's wss listener",
            Reachability::NoListener,
            Some(reason),
        ),
        WssOffer::Absent => Check::pass(
            "remote-reachable",
            "this server has no wss listener; nothing routable to probe",
        ),
        WssOffer::Bound { addr } => {
            let overlay = if bound_needs_overlay(addr.as_deref()) {
                phux_config::overlay::detect()
            } else {
                Vec::new()
            };
            remote_reachable_verdict(off_loopback_target(addr.as_deref(), &overlay), |url| {
                probe_remote_listener(url)
            })
        }
    }
}

/// What this instance's `GET_STATE` says about its wss listener.
#[derive(Debug, Clone)]
pub(super) enum WssOffer {
    /// A bound, healthy wss slot, with the address the server recorded.
    Bound { addr: Option<String> },
    /// A wss slot exists and the server disabled it.
    Disabled(phux_protocol::wire::ListenerDisabledReason),
    /// No server, no listener table, or no wss slot.
    Absent,
}

/// Where the off-loopback reachability probe should dial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OffLoopbackTarget {
    /// Concrete non-loopback address belonging to *this* server.
    Dial(SocketAddr),
    /// Bound on loopback only — the Application Firewall does not apply.
    LoopbackOnly,
    /// Unspecified or unknown bind, and no overlay IP to rewrite onto.
    NoOffLoopback { bound: String },
}

pub(super) fn bound_needs_overlay(bound: Option<&str>) -> bool {
    bound
        .and_then(|raw| raw.parse::<SocketAddr>().ok())
        .is_some_and(|addr| addr.ip().is_unspecified())
}

/// Pick a dial target from this server's bound wss address.
pub(super) fn off_loopback_target(bound: Option<&str>, overlay: &[IpAddr]) -> OffLoopbackTarget {
    let Some(raw) = bound else {
        return OffLoopbackTarget::NoOffLoopback {
            bound: "unknown".into(),
        };
    };
    let Ok(addr) = raw.parse::<SocketAddr>() else {
        return OffLoopbackTarget::NoOffLoopback {
            bound: raw.to_owned(),
        };
    };
    if addr.ip().is_loopback() {
        return OffLoopbackTarget::LoopbackOnly;
    }
    if addr.ip().is_unspecified() {
        return overlay
            .iter()
            .copied()
            .find(|ip| !ip.is_loopback() && !ip.is_unspecified() && ip.is_ipv4() == addr.is_ipv4())
            .map_or_else(
                || OffLoopbackTarget::NoOffLoopback {
                    bound: raw.to_owned(),
                },
                |ip| OffLoopbackTarget::Dial(SocketAddr::new(ip, addr.port())),
            );
    }
    OffLoopbackTarget::Dial(addr)
}

/// The pure half of the bound-address verdict.
pub(super) fn remote_reachable_verdict(
    target: OffLoopbackTarget,
    probe: impl FnOnce(&str) -> Reachability,
) -> Check {
    match target {
        OffLoopbackTarget::LoopbackOnly => Check::pass(
            "remote-reachable",
            "wss is bound on loopback only; inbound host-firewall rules do not apply",
        ),
        OffLoopbackTarget::NoOffLoopback { bound } => Check::warn(
            "remote-reachable",
            format!(
                "wss is bound on {bound}; no off-loopback address to probe, so a \
                 host firewall stealth-drop would look healthy"
            ),
            "doctor only probes a concrete non-loopback bind, or an overlay IP \
             rewritten onto 0.0.0.0/:: — bind `--listen` to a routable address, \
             or check PHUX_OVERLAY_ADDRS (for Tailscale, `tailscale ip -4`)",
        ),
        OffLoopbackTarget::Dial(addr) => {
            let url = format!("wss://{addr}");
            remote_reachable_check(&url, probe(&url), None)
        }
    }
}

/// Ask the running server what it is doing with wss.
pub(super) fn server_wss_offer(socket_path: &std::path::Path) -> WssOffer {
    if !socket_path.exists() {
        return WssOffer::Absent;
    }
    let Ok(rt) = cli_runtime() else {
        return WssOffer::Absent;
    };
    let Ok(view) = rt.block_on(phux_client::state::get_state(socket_path)) else {
        return WssOffer::Absent;
    };
    let Some(report) = view.snapshot().listeners() else {
        return WssOffer::Absent;
    };
    let Some(slot) = report
        .listeners
        .iter()
        .find(|slot| slot.transport == phux_protocol::wire::RemoteListenerTransport::Wss)
    else {
        return WssOffer::Absent;
    };
    if slot.is_unhealthy() {
        return slot
            .disabled_reason
            .map_or(WssOffer::Absent, WssOffer::Disabled);
    }
    if slot.bound {
        WssOffer::Bound {
            addr: slot.addr.clone(),
        }
    } else {
        WssOffer::Absent
    }
}

/// Dial `url` and classify the answer. No token and no cert verification:
/// this asks only whether packets land.
pub(super) fn probe_remote_listener(url: &str) -> Reachability {
    let Ok(runtime) = cli_runtime() else {
        return Reachability::Unreachable;
    };
    let dial = phux_dial::ws::WsDial {
        url: url.to_owned(),
        token: None,
        trust: phux_dial::CertTrust::SkipVerify,
        tls_server_name: None,
        identity: None,
    };
    let probe = async {
        let Ok(outcome) =
            tokio::time::timeout(REMOTE_PROBE_TIMEOUT, phux_dial::ws::dial(&dial)).await
        else {
            return Reachability::Silent;
        };
        match outcome {
            Err(phux_dial::DialError::Unreachable(err)) => {
                if err.to_lowercase().contains("refused") {
                    Reachability::NoListener
                } else {
                    Reachability::Unreachable
                }
            }
            Ok(_) | Err(_) => Reachability::Answered,
        }
    };
    runtime.block_on(probe)
}

/// The pure half of [`check_remote_reachable`].
pub(super) fn remote_reachable_check(
    url: &str,
    reachability: Reachability,
    server_disabled: Option<phux_protocol::wire::ListenerDisabledReason>,
) -> Check {
    match reachability {
        Reachability::Answered => Check::pass(
            "remote-reachable",
            format!("{url} answered; remote clients can reach this server"),
        ),
        Reachability::Silent => Check::fail(
            "remote-reachable",
            format!(
                "{url} accepted a connection and then answered nothing — UDS is healthy \
                 and the listener is bound, so this is a host firewall stealth-drop, not \
                 a dead server"
            ),
            FIREWALL_REMEDY,
        ),
        Reachability::NoListener => {
            let hint = match server_disabled {
                Some(phux_protocol::wire::ListenerDisabledReason::TokenStoreLoadFailed) => {
                    "the running server disabled wss because the credential store failed to \
                     load at boot — fix the store, then restart or `phux upgrade`"
                        .to_owned()
                }
                Some(phux_protocol::wire::ListenerDisabledReason::PolicyModeRefused) => {
                    "the running server left wss unbound because no [policy] mode admits it — \
                     set [policy] mode = \"paired\" in config.toml, then `phux upgrade`"
                        .to_owned()
                }
                Some(reason) => {
                    format!(
                        "the running server reports wss disabled ({}) — check the server log, \
                         then restart or `phux upgrade` after fixing it",
                        reason.as_str()
                    )
                }
                None => "expected remote access? set [policy] mode = \"paired\" in config.toml — \
                     with no mode the server binds no remote listener, configured or \
                     auto-bound — then `phux pair`"
                    .to_owned(),
            };
            Check::warn(
                "remote-reachable",
                format!("nothing is listening on {url}"),
                hint,
            )
        }
        Reachability::Unreachable => Check::warn(
            "remote-reachable",
            format!("could not reach {url} from this host"),
            "if this is a VPN address, check its tunnel and routes (Tailscale: `tailscale status`)",
        ),
    }
}

/// What to do about a listener that is bound but unreachable. On macOS the
/// Application Firewall drops packets to the adhoc-signed binary, and its
/// allowlist is keyed to a path that changes on every Homebrew upgrade.
#[cfg(target_os = "macos")]
pub(super) const FIREWALL_REMEDY: &str = "macOS: Application Firewall stealth-drops inbound packets to \
     unrecognized binaries, and phux is adhoc-signed. Allowlisting is per exact path \
     (Homebrew `/opt/homebrew/Cellar/phux/<version>/bin/phux`), so every upgrade breaks it. \
     Check with `/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate`. On a \
     host that already lives behind an overlay, turning the firewall off is the durable \
     workaround until signed/notarized releases";

#[cfg(not(target_os = "macos"))]
pub(super) const FIREWALL_REMEDY: &str = "check this host's packet filter for a rule dropping inbound \
     connections to phux's listener port";

#[cfg(test)]
mod tests;
