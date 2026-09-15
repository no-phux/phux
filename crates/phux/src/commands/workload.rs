//! `phux workload` — the mTLS workload authority (ADR-0116,
//! `docs/spec/workload-auth.md` §8).
//!
//! `authority` fingerprints the workload CA and, with `--init`, creates it.
//! `add-key` enrolls a client certificate this CA issued, or signs a
//! certificate signing request into one; the material is read from stdin or
//! from the file `--file` names, never from the command line. `list` and
//! `revoke` show and retire credentials. Like `phux pair`, these write the
//! state directory directly and never contact a server: a running server
//! observes each new registry generation on its next connection.
//!
//! Output is limited to credential ids, scope ceilings, expiries, revocation
//! state, public keys on request, and the CA fingerprint. Nothing here
//! accepts, reads, or prints a private key; the CA key's path is never
//! printed, and neither is any path the user supplied (a value pasted into
//! the wrong flag could be key material).

use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::{DateTime, SecondsFormat, Utc};
use phux_server::workload::{
    self, ClientMaterial, MAX_MATERIAL_BYTES, MaterialError, PreparedEnrollment,
    WorkloadCredential, WorkloadError, WorkloadPaths, WorkloadRegistry,
};
use serde_json::json;
use usage::Subcommands;

use super::json_err::{self, CliError, codes};

/// Default credential lifetime: 90 days.
const DEFAULT_EXPIRY_SECONDS: i64 = 90 * 24 * 60 * 60;

#[derive(Debug, Subcommands)]
pub(crate) enum WorkloadAction {
    /// Print the workload CA fingerprint.
    ///
    /// Prints only the `sha256:` fingerprint clients pin. `--init` creates
    /// the CA first when none exists; an existing CA is never replaced.
    Authority {
        /// Create the CA if it does not exist yet.
        #[usage(long)]
        init: bool,
    },
    /// Enroll a client certificate, or sign a CSR into one.
    ///
    /// Reads one PEM client certificate issued by this authority, or one PEM
    /// certificate signing request, from stdin or from `--file`. Key material
    /// is never taken from the command line, and input that contains a
    /// private key is refused. A CSR is signed into a client certificate
    /// written to the new file `--cert-out` names.
    AddKey {
        /// Read the certificate or CSR from this file instead of stdin.
        #[usage(long, value_name = "PATH")]
        file: Option<PathBuf>,

        /// Scope ceiling as `<verb>[,<verb>...]@<selector>`, where a verb is
        /// inventory, observe, create, bind, input, signal, or `*`, and a
        /// selector is global, host, host:NAME, group:ID, terminal:ID, or
        /// terminal:HOST/ID. Repeatable; at least one is required.
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

        /// New file to write the certificate issued for a CSR to. Required
        /// with a CSR; never overwrites an existing file.
        #[usage(long, value_name = "PATH")]
        cert_out: Option<PathBuf>,
    },
    /// List enrolled credentials.
    List {
        /// Also print each credential's public key (hex DER).
        #[usage(long)]
        public_keys: bool,
    },
    /// Revoke a credential for new connections.
    Revoke {
        /// Credential id (`sha256:...`) printed by `add-key` or `list`.
        #[usage(value_name = "CREDENTIAL_ID")]
        credential_id: String,
    },
}

/// `phux workload ...`. Exit 0 on success, 1 on any refusal or failure.
pub(crate) fn run(action: WorkloadAction, json: bool) -> ExitCode {
    let paths = WorkloadPaths::from_env();
    let outcome = match action {
        WorkloadAction::Authority { init } => authority(&paths, init),
        WorkloadAction::AddKey {
            file,
            scope,
            expires_in,
            cert_out,
        } => add_key(
            &paths,
            &AddKey {
                file,
                scopes: scope,
                expires_in,
                cert_out,
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

/// A successful action's output: one JSON document, or prose lines.
struct Report {
    document: serde_json::Value,
    prose: Vec<String>,
}

impl Report {
    fn print(&self, json: bool) -> ExitCode {
        if !json {
            for line in &self.prose {
                outln!("{line}");
            }
            return ExitCode::SUCCESS;
        }
        match serde_json::to_string_pretty(&self.document) {
            Ok(text) => {
                outln!("{text}");
                ExitCode::SUCCESS
            }
            Err(error) => json_err::emit(
                true,
                &CliError::new(
                    codes::JSON_SERIALIZE,
                    format!("could not encode JSON: {error}"),
                    "",
                ),
                1,
            ),
        }
    }
}

fn authority(paths: &WorkloadPaths, init: bool) -> Result<Report, CliError> {
    let status = if init {
        workload::init_authority(&paths.ca_cert, &paths.ca_key)
    } else {
        workload::ca_fingerprint(&paths.ca_cert).map(|fingerprint| workload::AuthorityStatus {
            fingerprint,
            created: false,
        })
    }
    .map_err(|error| cli_error(&error))?;
    Ok(Report {
        document: json!({
            "schema_version": 1,
            "ca_fingerprint": status.fingerprint,
            "created": status.created,
        }),
        prose: vec![status.fingerprint],
    })
}

struct AddKey {
    file: Option<PathBuf>,
    scopes: Vec<String>,
    expires_in: i64,
    cert_out: Option<PathBuf>,
}

fn add_key(paths: &WorkloadPaths, args: &AddKey) -> Result<Report, CliError> {
    workload::validate_scopes(&args.scopes).map_err(|error| cli_error(&error))?;
    let material = read_material(args.file.as_deref())?;
    check_cert_out(&material, args.cert_out.as_deref())?;
    let expires_at = Utc::now().timestamp().saturating_add(args.expires_in);
    let prepared = workload::prepare_enrollment(paths, &material, expires_at)
        .map_err(|error| cli_error(&error))?;
    let written = write_issued(&prepared, args.cert_out.as_deref())?;
    let registered = prepared
        .commit(&paths.registry, args.scopes.clone(), expires_at)
        .map_err(|error| {
            // Nothing admits a certificate whose key the commit refused.
            if let Some(path) = &written {
                let _ = fs::remove_file(path);
            }
            cli_error(&error)
        })?;
    let mut prose = vec![
        format!("Enrolled {}", registered.id),
        format!("  scopes:      {}", args.scopes.join(" ")),
        format!("  expires:     {}", timestamp(expires_at)),
        format!("  generation:  {}", registered.generation),
    ];
    if written.is_some() {
        prose.push("  certificate: written to the --cert-out file".to_owned());
    }
    Ok(Report {
        document: json!({
            "schema_version": 1,
            "operation": "add-key",
            "credential_id": registered.id,
            "material": material.kind(),
            "scopes": args.scopes,
            "expires_at": expires_at,
            "registry_generation": registered.generation,
            "certificate_written": written.is_some(),
        }),
        prose,
    })
}

/// Enrollment input is public by contract, but a mistaken pipe can carry a
/// private key: the buffer is preallocated (so no reallocation leaves a copy
/// behind) and overwritten when dropped, on every path.
struct Scrubbed(Vec<u8>);

impl Drop for Scrubbed {
    fn drop(&mut self) {
        workload::scrub(&mut self.0);
    }
}

fn read_material(file: Option<&Path>) -> Result<ClientMaterial, CliError> {
    let input = match file {
        // The path is not echoed: a value pasted into the wrong flag must
        // not come back out on stderr.
        Some(path) => fs::File::open(path)
            .map_err(|error| material_error(format!("cannot open the --file path: {error}")))
            .and_then(read_bounded)?,
        None => read_stdin()?,
    };
    ClientMaterial::from_pem(&input.0).map_err(|error| cli_error(&WorkloadError::from(error)))
}

fn read_stdin() -> Result<Scrubbed, CliError> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Err(CliError::new(
            codes::WORKLOAD,
            "add-key reads a certificate or CSR on stdin, and stdin is a terminal",
            "pipe it in (`phux workload add-key --scope observe@global --cert-out client.pem < client.csr`) or pass --file PATH",
        ));
    }
    read_bounded(stdin.lock())
}

fn read_bounded(reader: impl Read) -> Result<Scrubbed, CliError> {
    let limit = MAX_MATERIAL_BYTES + 1;
    let mut buffer = Scrubbed(Vec::with_capacity(limit));
    reader
        .take(u64::try_from(limit).unwrap_or(u64::MAX))
        .read_to_end(&mut buffer.0)
        .map_err(|error| material_error(format!("cannot read enrollment material: {error}")))?;
    Ok(buffer)
}

fn material_error(message: String) -> CliError {
    CliError::new(codes::WORKLOAD, message, "")
}

fn check_cert_out(material: &ClientMaterial, cert_out: Option<&Path>) -> Result<(), CliError> {
    match (material, cert_out) {
        (ClientMaterial::Request(_), None) => Err(CliError::new(
            codes::WORKLOAD,
            "a CSR is signed into a new client certificate, which needs somewhere to go",
            "name a new file for it with --cert-out PATH",
        )),
        (ClientMaterial::Certificate { .. }, Some(_)) => Err(CliError::new(
            codes::WORKLOAD,
            "--cert-out applies only to a CSR; a supplied certificate is enrolled as it is",
            "drop --cert-out",
        )),
        _ => Ok(()),
    }
}

/// Write the certificate issued for a CSR to a new file, before the registry
/// commit: a failed commit then leaves nothing enrolled (and the file is
/// removed), never an enrolled key whose certificate was lost.
fn write_issued(
    prepared: &PreparedEnrollment,
    cert_out: Option<&Path>,
) -> Result<Option<PathBuf>, CliError> {
    let (Some(chain), Some(path)) = (prepared.issued_chain_pem(), cert_out) else {
        return Ok(None);
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)
        .map_err(|error| cert_out_error(&error))?;
    if let Err(error) = file
        .write_all(chain.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(path);
        return Err(cert_out_error(&error));
    }
    Ok(Some(path.to_owned()))
}

/// The path is deliberately not echoed: a value pasted into `--cert-out`
/// could be key material.
fn cert_out_error(error: &io::Error) -> CliError {
    CliError::new(
        codes::WORKLOAD,
        format!("cannot write the issued certificate to the --cert-out path: {error}"),
        "name a new file with --cert-out; an existing file is never overwritten",
    )
}

fn list(paths: &WorkloadPaths, public_keys: bool) -> Result<Report, CliError> {
    let registry = WorkloadRegistry::load(&paths.registry).map_err(|error| cli_error(&error))?;
    let now = Utc::now().timestamp();
    let credentials = registry.credentials();
    let mut prose = vec![format!(
        "registry generation {}, {} credential(s)",
        registry.generation(),
        credentials.len()
    )];
    for credential in credentials {
        prose.extend(credential_lines(credential, now, public_keys));
    }
    let documents: Vec<serde_json::Value> = credentials
        .iter()
        .map(|credential| credential_document(credential, now, public_keys))
        .collect();
    Ok(Report {
        document: json!({
            "schema_version": 1,
            "registry_instance": registry.instance_id(),
            "registry_generation": registry.generation(),
            "credentials": documents,
        }),
        prose,
    })
}

fn status(credential: &WorkloadCredential, now: i64) -> &'static str {
    if credential.revoked_at.is_some() {
        "revoked"
    } else if credential.is_active_at(now) {
        "active"
    } else {
        "expired"
    }
}

fn credential_lines(credential: &WorkloadCredential, now: i64, public_keys: bool) -> Vec<String> {
    let expires = credential
        .expires_at
        .map_or_else(|| "never".to_owned(), timestamp);
    let mut lines = vec![
        format!("{}  {}", credential.id, status(credential, now)),
        format!("  scopes:  {}", credential.scopes.join(" ")),
        format!("  expires: {expires}"),
    ];
    if let Some(revoked_at) = credential.revoked_at {
        lines.push(format!("  revoked: {}", timestamp(revoked_at)));
    }
    if public_keys {
        lines.push(format!("  public key: {}", hex(&credential.public_key)));
    }
    lines
}

fn credential_document(
    credential: &WorkloadCredential,
    now: i64,
    public_keys: bool,
) -> serde_json::Value {
    let mut document = json!({
        "credential_id": credential.id,
        "status": status(credential, now),
        "scopes": credential.scopes,
        "expires_at": credential.expires_at,
        "revoked_at": credential.revoked_at,
    });
    if public_keys {
        document["public_key"] = json!(hex(&credential.public_key));
    }
    document
}

fn revoke(paths: &WorkloadPaths, credential_id: &str) -> Result<Report, CliError> {
    let outcome = WorkloadRegistry::revoke(&paths.registry, credential_id)
        .map_err(|error| cli_error(&error))?;
    let prose = if outcome.newly_revoked {
        vec![
            format!(
                "Revoked {credential_id} (registry generation {}).",
                outcome.generation
            ),
            "A running server refuses it from its next connection attempt; no restart is needed."
                .to_owned(),
        ]
    } else {
        vec![format!(
            "{credential_id} was already revoked at {}.",
            timestamp(outcome.revoked_at)
        )]
    };
    Ok(Report {
        document: json!({
            "schema_version": 1,
            "operation": "revoke",
            "credential_id": credential_id,
            "revoked_at": outcome.revoked_at,
            "newly_revoked": outcome.newly_revoked,
            "registry_generation": outcome.generation,
        }),
        prose,
    })
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
            "persist a global, host, or host:<name> selector; session and Terminal ids restart with the server"
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

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}
