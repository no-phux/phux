//! `phux workload add-key`: enroll a supplied certificate or sign a CSR,
//! read from stdin or `--file`, never argv.

use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::Utc;
use phux_server::workload::{
    self, ClientMaterial, MAX_MATERIAL_BYTES, PreparedEnrollment, WorkloadError, WorkloadPaths,
};
use serde_json::json;

use super::{Report, cli_error, timestamp};
use crate::commands::json_err::{CliError, codes};

/// Where the certificate issued for a CSR goes.
pub(super) enum IssuedTo {
    /// `--cert-out PATH`: a new file.
    File(PathBuf),
    /// `--cert-stdout`: the report's payload.
    Stdout,
    /// Neither flag: right for a supplied certificate, refused for a CSR.
    Nowhere,
}

impl IssuedTo {
    fn file(&self) -> Option<&Path> {
        match self {
            Self::File(path) => Some(path),
            Self::Stdout | Self::Nowhere => None,
        }
    }
}

pub(super) struct AddKey {
    pub(super) file: Option<PathBuf>,
    pub(super) scopes: Vec<String>,
    pub(super) expires_in: i64,
    pub(super) issued: IssuedTo,
    pub(super) replace: Option<String>,
    pub(super) principal: Option<workload::EnrolledFor>,
}

pub(super) fn add_key(paths: &WorkloadPaths, args: &AddKey) -> Result<Report, CliError> {
    workload::validate_scopes(&args.scopes).map_err(|error| cli_error(&error))?;
    if let Some(replace) = &args.replace
        && !workload::is_canonical_credential_id(replace)
    {
        return Err(cli_error(&WorkloadError::InvalidCredentialId));
    }
    let material = read_material(args.file.as_deref())?;
    check_cert_out(&material, &args.issued)?;
    let expires_at = Utc::now().timestamp().saturating_add(args.expires_in);
    let prepared = workload::prepare_enrollment(paths, &material, expires_at)
        .map_err(|error| cli_error(&error))?;
    let written = write_issued(&prepared, args.issued.file())?;
    let registered = prepared
        .commit_replacing(
            &paths.registry,
            args.scopes.clone(),
            expires_at,
            args.replace.as_deref(),
            args.principal,
        )
        .map_err(|error| {
            // Nothing admits a certificate whose key the commit refused.
            if let Some(path) = &written {
                let _ = fs::remove_file(path);
            }
            cli_error(&error)
        })?;
    let enrolled_for = args
        .principal
        .unwrap_or_else(|| workload::EnrolledFor::by_ceiling(&args.scopes));
    let mut prose = vec![
        format!("Enrolled {}", registered.id),
        format!("  scopes:      {}", args.scopes.join(" ")),
        format!("  principal:   {}", enrolled_for.as_str()),
        format!("  expires:     {}", timestamp(expires_at)),
        format!("  generation:  {}", registered.generation),
    ];
    if written.is_some() {
        prose.push("  certificate: written to the --cert-out file".to_owned());
    }
    if registered.replaced
        && let Some(replaced) = &args.replace
    {
        prose.push(format!("  replaced:    {replaced} (revoked)"));
    }
    let payload = matches!(args.issued, IssuedTo::Stdout)
        .then(|| prepared.issued_chain_pem().map(str::to_owned))
        .flatten();
    let mut document = json!({
        "schema_version": 1,
        "operation": "add-key",
        "credential_id": registered.id,
        "material": material.kind(),
        "scopes": args.scopes,
        "principal": enrolled_for.as_str(),
        "expires_at": expires_at,
        "registry_generation": registered.generation,
        "certificate_written": written.is_some(),
        "replaced": registered.replaced,
    });
    if let Some(chain) = &payload {
        document["certificate_chain"] = json!(chain);
    }
    Ok(Report {
        document,
        prose,
        payload,
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

fn check_cert_out(material: &ClientMaterial, issued: &IssuedTo) -> Result<(), CliError> {
    match (material, issued) {
        (ClientMaterial::Request(_), IssuedTo::Nowhere) => Err(CliError::new(
            codes::WORKLOAD,
            "a CSR is signed into a new client certificate, which needs somewhere to go",
            "name a new file for it with --cert-out PATH, or print it with --cert-stdout",
        )),
        (ClientMaterial::Certificate { .. }, IssuedTo::File(_) | IssuedTo::Stdout) => {
            Err(CliError::new(
                codes::WORKLOAD,
                "--cert-out and --cert-stdout apply only to a CSR; a supplied certificate is enrolled as it is",
                "drop --cert-out / --cert-stdout",
            ))
        }
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
