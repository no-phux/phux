//! The shared ssh middle of `phux host add HOST` (ADR-0055, ADR-0066,
//! ADR-0122): bring another machine's server up and pair with it over ssh.
//! Enrollment grants no authority ssh did not already grant; it removes the
//! hand-copying of a token and a fingerprint.
//!
//! Over one ssh channel: confirm `phux` is installed; make sure a server runs
//! and stays running (install the service, adopt a live server, or fall back to
//! `phux server --ensure`); run `phux pair --json`; probe the direct routes;
//! and return the first that answers, or `ssh://HOST` with the first
//! candidate kept as `direct`. A host with nothing
//! dialable is not an error.
//!
//! A remote also gets a workload client certificate (ADR-0116): the key is
//! generated here, only its CSR crosses ssh (on stdin, never argv) to
//! `phux workload add-key --cert-stdout`, and the returned chain is checked
//! against the key before both are stored owner-only. The probes present it.
//! Replacing a certificate is two-phase ([`settle_identity`]): the credential
//! it supersedes is revoked on the far host only after the registry entry
//! names the new pair, and a new credential nothing recorded is revoked
//! instead, so no failure strands the entry on a revoked certificate.
//!
//! Nothing here prints or writes a registry: callers render the events, and
//! the role-specific tails in `host` own the token path and the entry.

mod events;
mod flow;
mod identity;
mod pair;
mod server;
mod ssh;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_pair;
mod workload;

pub(crate) use events::{EnrollEvent, EnrollFailure, ServicePolicy, Supervision};
pub(crate) use flow::{EnrollOutcome, FarHost, HOST_ADD_WORKLOAD_SCOPE, enroll_over_ssh};
pub(crate) use identity::{
    CertificateStatus, ClientIdentityFiles, EnrollRequest, HeldIdentity, WorkloadEnrollment,
    admission_ends, calendar_date, renewal_due,
};
pub(crate) use pair::{
    PairReport, default_quic_port, probe, probe_deadline, probe_within, satellite_token_path,
    token_path, write_token,
};
pub(crate) use server::ensure_remote_server;
pub(crate) use ssh::{authority, ssh_hostname, ssh_program};
pub(crate) use workload::{
    Settlement, TicketPins, enroll_client_certificate, enroll_with_ticket, settle_identity,
};

/// `phux service install`'s refusal when a live server holds the socket;
/// enrollment answers it with `--adopt`.
const SERVICE_INCUMBENT_LIVE: &str = "a server is already running on";
