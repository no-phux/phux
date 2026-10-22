//! `phux workload` — the mTLS workload authority (ADR-0116,
//! `docs/spec/workload-auth.md` §8): `authority [--init|--rotate]`, `add-key` (enroll a
//! certificate or sign a CSR, read from stdin or `--file`, never argv), `list`,
//! and `revoke`. Like `phux pair` these write the state directory directly;
//! a running server sees the new registry generation on its next connection.
//! Nothing here reads or prints a private key, the CA key's path, or any
//! user-supplied path (a mis-pasted value could be key material).

use std::path::PathBuf;
use std::process::ExitCode;

use chrono::{DateTime, SecondsFormat, Utc};
use phux_server::workload::{self, MaterialError, WorkloadError, WorkloadPaths};
use usage::{Subcommands, ValueEnum};

use super::json_err::{self, CliError, codes};

mod add_key;
mod registry;

use self::add_key::{AddKey, IssuedTo, add_key};
use self::registry::{authority, list, revoke, rotate};

/// Default credential lifetime: 90 days.
const DEFAULT_EXPIRY_SECONDS: i64 = 90 * 24 * 60 * 60;

/// Who an enrolled credential is for (ADR-0163 §6): `--principal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Principal {
    /// A device a person uses: its takeover of a terminal's input beats an
    /// agent's, and no agent can take input from it.
    Human,
    /// An unattended workload: a non-human principal.
    Automation,
}

impl From<Principal> for workload::EnrolledFor {
    fn from(principal: Principal) -> Self {
        match principal {
            Principal::Human => Self::Human,
            Principal::Automation => Self::Automation,
        }
    }
}

#[derive(Debug, Subcommands)]
pub(crate) enum WorkloadAction {
    /// Print the workload CA fingerprint.
    ///
    /// Prints only the `sha256:` fingerprint clients pin. `--init` creates
    /// the CA first when none exists; an existing CA is never replaced.
    /// `--rotate` replaces it with a new CA and re-issues the server
    /// certificate under it: every client then refuses this server, naming
    /// both fingerprints, until it re-pairs.
    Authority {
        /// Create the CA if it does not exist yet.
        #[usage(long, conflicts("--rotate"))]
        init: bool,

        /// Replace the CA with a new one and re-issue the server
        /// certificate under it. Every client must re-pair, and
        /// every workload credential re-enroll; the replaced files are kept
        /// beside the new ones as `*.retired-<unix>`. Restart the server to
        /// present it.
        #[usage(long)]
        rotate: bool,
    },
    /// Enroll a client certificate, or sign a CSR into one.
    ///
    /// Reads one PEM client certificate issued by this authority, or one PEM
    /// certificate signing request, from stdin or from `--file`. Key material
    /// is never taken from the command line, and input that contains a
    /// private key is refused. A CSR is signed into a client certificate
    /// written to the new file `--cert-out` names, or printed with
    /// `--cert-stdout`.
    AddKey {
        /// Read the certificate or CSR from this file instead of stdin.
        #[usage(long, value_name = "PATH")]
        file: Option<PathBuf>,

        /// Scope ceiling as `<verb>[,<verb>...]@<selector>`, where a verb is
        /// inventory, observe, create, bind, input, signal, organize,
        /// terminate, or `*`, and a selector is global, host, host:NAME,
        /// session:KEY, or resource:KEY (a record's full 26-character key,
        /// the `key` of a `phux ls --json` row; session and Terminal ids
        /// restart with the server, so group:ID and terminal:ID are
        /// refused). Repeatable; at least one is required.
        #[usage(long, value_name = "VERBS@SELECTOR")]
        scope: Vec<String>,

        /// Seconds until the credential expires (at most 20 years).
        #[usage(
            long,
            default = "7776000", default_value_t = DEFAULT_EXPIRY_SECONDS,
            validate = "int(value) >= 60 && int(value) <= 630720000", validate_error = "must be between 60 and 630720000 seconds (20 years)",
            value_name = "SECONDS"
        )]
        expires_in: i64,

        /// New file to write the certificate issued for a CSR to. A CSR
        /// needs this or `--cert-stdout`; never overwrites an existing file.
        #[usage(long, value_name = "PATH", conflicts("--cert-stdout"))]
        cert_out: Option<PathBuf>,

        /// Print the certificate chain issued for a CSR (leaf, then the CA)
        /// on stdout instead of writing a file: as PEM, or as the
        /// `certificate_chain` field of the `--json` document. This is how
        /// `phux host add` enrolls over ssh; the chain is public.
        #[usage(long)]
        cert_stdout: bool,

        /// Revoke this credential (`sha256:...`) in the same registry write
        /// that enrolls the new one, so a re-enrolled client never holds two
        /// live credentials. One the registry does not hold is ignored.
        #[usage(long, value_name = "CREDENTIAL_ID")]
        replace: Option<String>,

        /// Who the credential is for: `human` (a device a person uses) or
        /// `automation`. Decides input-lease precedence, never scope.
        /// Omitted, the credential is a person's device when its ceiling is
        /// exactly the one `phux host add` and `phux pair --enroll` grant,
        /// and an automation otherwise.
        #[usage(long, value_enum)]
        principal: Option<Principal>,
    },
    /// List enrolled credentials.
    List {
        /// Also print each credential's public key (hex DER).
        #[usage(long)]
        public_keys: bool,
    },
    /// Revoke a credential: new connections are refused and live ones end.
    ///
    /// A running server observes the registry write and closes every
    /// connection the credential admitted; no restart is needed. `phux pair
    /// revoke` accepts the same `sha256:` ids. A satellite's credential ends
    /// its link to this hub. A `t-...` ticket id withdraws an enrollment code
    /// nobody has used yet.
    Revoke {
        /// Credential id (`sha256:...`) printed by `add-key` or `list`, or
        /// an unspent enrollment ticket (`t-...`) printed by `phux pair
        /// --enroll` or `phux host ticket`.
        #[usage(value_name = "CREDENTIAL_ID")]
        credential_id: String,
    },
}

/// `phux workload ...`. Exit 0 on success, 1 on any refusal or failure.
pub(crate) fn run(action: WorkloadAction, json: bool) -> ExitCode {
    let paths = WorkloadPaths::from_env();
    let outcome = match action {
        WorkloadAction::Authority {
            init: _,
            rotate: true,
        } => rotate(&paths),
        WorkloadAction::Authority {
            init,
            rotate: false,
        } => authority(&paths, init),
        WorkloadAction::AddKey {
            file,
            scope,
            expires_in,
            cert_out,
            cert_stdout,
            replace,
            principal,
        } => add_key(
            &paths,
            &AddKey {
                file,
                scopes: scope,
                expires_in,
                issued: match cert_out {
                    Some(path) => IssuedTo::File(path),
                    None if cert_stdout => IssuedTo::Stdout,
                    None => IssuedTo::Nowhere,
                },
                replace,
                principal: principal.map(workload::EnrolledFor::from),
            },
        ),
        WorkloadAction::List { public_keys } => list(&paths, public_keys),
        WorkloadAction::Revoke { credential_id } => revoke(&paths, &credential_id),
    };
    match outcome {
        Ok(report) => report.print(json),
        Err(error) => json_err::emit(json, &error, 1),
    }
}

/// A successful action's output: one JSON document, or prose lines. With
/// `payload` (an issued chain for `--cert-stdout`), the prose goes to stderr
/// so stdout carries the PEM alone.
struct Report {
    document: serde_json::Value,
    prose: Vec<String>,
    payload: Option<String>,
}

impl Report {
    fn print(&self, json: bool) -> ExitCode {
        if json {
            return crate::output::json(&self.document);
        }
        match &self.payload {
            Some(payload) => {
                for line in &self.prose {
                    info!("{line}");
                }
                out!("{payload}");
            }
            None => {
                for line in &self.prose {
                    outln!("{line}");
                }
            }
        }
        ExitCode::SUCCESS
    }
}

/// Map a workload failure to the CLI error vocabulary, with the way out.
fn cli_error(error: &WorkloadError) -> CliError {
    let remedy = match error {
        WorkloadError::AuthorityMissing => "run `phux workload authority --init` first",
        WorkloadError::NotIssuedByAuthority => {
            "enroll a CSR instead, and this authority issues the certificate"
        }
        WorkloadError::Material(MaterialError::PrivateKey) => {
            "generate a CSR beside the key (`openssl req -new -key KEY -subj /CN=workload`) and pipe that"
        }
        WorkloadError::InvalidScope { .. } | WorkloadError::NoScopes => {
            "pass --scope VERBS@SELECTOR, for example --scope observe,input@host"
        }
        WorkloadError::UnstableSelector { .. } => {
            "name a session or Terminal by its record key (session:<key> or resource:<key>, the `key` `phux ls --json` prints); session and Terminal ids restart with the server"
        }
        WorkloadError::Insecure { .. } | WorkloadError::PartialPair { .. } => {
            "workload authority files are owner-only regular files in a directory only their owner can write"
        }
        _ => "",
    };
    CliError::new(codes::WORKLOAD, error.to_string(), remedy)
}

fn timestamp(seconds: i64) -> String {
    DateTime::from_timestamp(seconds, 0).map_or_else(
        || seconds.to_string(),
        |at: DateTime<Utc>| at.to_rfc3339_opts(SecondsFormat::Secs, true),
    )
}
