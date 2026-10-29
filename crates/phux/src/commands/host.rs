//! `phux host` — one namespace over the two machine registries (ADR-0066,
//! ADR-0122). `--role remote` (default) operates on `[[remote]]`, servers this
//! machine dials; `--role satellite` on `[[satellites]]`, peers a hub dials for
//! its users. Storage stays split along that trust direction; everything
//! delegates to [`super::remote`] and [`super::satellite`].
//!
//! `phux host add [USER@]HOST` sets a machine up over ssh end to end
//! ([`super::enroll::enroll_over_ssh`]); `NAME ENDPOINT` or an endpoint URI
//! registers credentials minted elsewhere.
//!
//! `host ls --json` emits one stable document (`schema_version` 1):
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "hosts": [
//!     {
//!       "name": "mini",
//!       "role": "remote",
//!       "endpoint": "quic://mini:8788",
//!       "enabled": null,
//!       "token_file": "/path/to/token",
//!       "cert_fingerprint": "ab..64 hex..",
//!       "session": null,
//!       "ssh": "me@mini",
//!       "direct": null,
//!       "client_cert": "/path/to/mini.client.0123456789abcdef.pem",
//!       "client_key": "/path/to/mini.client.0123456789abcdef.key"
//!     }
//!   ]
//! }
//! ```
//!
//! `enabled` is `null` for remotes; `session`, `ssh`, `direct`, `client_cert`,
//! and `client_key` are `null` for satellites. The client identity is named
//! by path only; the key bytes never appear. `host add --json` and `host
//! renew --json` wrap one such object under `"host"`, beside an
//! `"enrollment"` object (`status`, `error`, `credential_id`, `expires_at`,
//! `previous_credential_id`, `previous_revoked`, `warnings`; documented in
//! `docs/reference/cli.md#phux-host-add`) saying what happened to the workload client
//! certificate.
//! `host rm --json` emits `{"schema_version":1,"removed":{"name":..,"role":..}}`.
//!
//! `phux host renew NAME` enrolls a fresh workload client certificate for a
//! registered remote over ssh; like a re-enrolling `host add`, it records
//! the new pair before revoking the old credential ([`enroll::settle_identity`]).
//! `phux host upgrade [NAME]` moves remotes whose QUIC route answers onto
//! it ([`super::remote_target::upgrade_to_quic`]); `--json` emits
//! `{"schema_version":1,"hosts":[{"name","status","previous_endpoint",
//! "endpoint","candidate","error"}]}`.
//! Failures follow the shared JSON error contract in [`super::json_err`].

use std::process::ExitCode;

use usage::ValueEnum;

mod add;
mod add_ssh;
mod args;
mod discover;
mod enrollment;
mod hub;
mod list;
mod manage;
mod minimal;
mod renew;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_add;

use add::run_add;
pub(crate) use args::HostAction;
use discover::run_discover;
pub(crate) use enrollment::{PreviousEnrollment, enroll_remote_over_ssh};
use list::run_list;
use manage::{run_attach, run_enabled, run_remove, run_rename, run_show};
use renew::{run_renew, run_upgrade};

mod inbound;

/// Which machine registry a `phux host` operation applies to: the flag names
/// the trust direction, the verb stays one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub(crate) enum HostRole {
    /// A server this machine attaches to — a `[[remote]]` entry.
    Remote,
    /// A peer this hub dials for its users — a `[[satellites]]` entry.
    Satellite,
}

impl HostRole {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Satellite => "satellite",
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::Remote => Self::Satellite,
            Self::Satellite => Self::Remote,
        }
    }

    /// The `--role` flag to paste into a remedy, empty for the default.
    const fn flag(self) -> &'static str {
        match self {
            Self::Remote => "",
            Self::Satellite => " --role satellite",
        }
    }
}

pub(crate) fn run_host(action: &HostAction) -> ExitCode {
    match action {
        HostAction::Add {
            target,
            endpoint,
            opts,
        } => run_add(target, endpoint.as_deref(), opts),
        HostAction::List { role, json } => run_list(*role, json.json),
        HostAction::Discover { login, json } => run_discover(*login, json.json),
        HostAction::Remove { name, role, json } => run_remove(name, *role, json.json),
        HostAction::Show { name, role, json } => run_show(name, *role, json.json),
        HostAction::Rename {
            name,
            new_name,
            role,
            json,
        } => run_rename(name, new_name, *role, json.json),
        HostAction::Renew {
            name,
            remote_phux,
            json,
        } => run_renew(name, remote_phux, json.json),
        HostAction::Upgrade { name, json } => run_upgrade(name.as_deref(), json.json),
        HostAction::Attach { name } => run_attach(name),
        HostAction::Enable { name, json } => run_enabled(name, true, json.json),
        HostAction::Disable { name, json } => run_enabled(name, false, json.json),
        HostAction::Ticket {
            name,
            hub,
            expires_in,
            mail_only,
            json,
        } => inbound::run_ticket(name, hub.as_deref(), *expires_in, *mail_only, json.json),
        HostAction::Join {
            code,
            mail_only,
            json,
        } => inbound::run_join(code, *mail_only, json.json),
        HostAction::Minimal { action } => minimal::run(action),
    }
}
