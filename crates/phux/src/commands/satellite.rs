mod json;
// `pub(crate)` (not private): `phux host` — the ADR-0066 umbrella verb —
// delegates to this registry module rather than growing a merged store.
pub(crate) mod registry;

use std::process::ExitCode;

use crate::commands::SatelliteAction;
use json::{print_satellite_json, print_satellites_json};
use registry::{NewSatellite, add_or_update, find_entry, load_registry, remove_entry};

pub(crate) fn run_satellite(action: &SatelliteAction) -> ExitCode {
    match action {
        SatelliteAction::List { json } => run_list(*json),
        SatelliteAction::Enroll {
            host,
            name,
            endpoint,
            quic_port,
            no_service,
            ssh_only,
            json,
        } => run_enroll(
            host,
            name.as_deref(),
            endpoint.as_deref(),
            *quic_port,
            !no_service,
            *ssh_only,
            *json,
        ),
        SatelliteAction::Add {
            name,
            endpoint,
            disabled,
            token_file,
            cert_fingerprint,
            json,
        } => run_add(
            NewSatellite::new(
                name,
                endpoint,
                !disabled,
                token_file.as_deref(),
                cert_fingerprint.as_deref(),
            ),
            *json,
        ),
        SatelliteAction::Remove { name, json } => run_remove(name, *json),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "CLI entry point; every argument is one documented enrollment choice"
)]
fn run_enroll(
    ssh_host: &str,
    name: Option<&str>,
    endpoint_override: Option<&str>,
    quic_port: u16,
    install_service: bool,
    ssh_only: bool,
    json: bool,
) -> ExitCode {
    let name = name.map_or_else(
        || crate::commands::enroll::default_name(ssh_host),
        str::to_owned,
    );

    if ssh_only {
        let endpoint = format!("ssh://{ssh_host}");
        return finish_enroll(NewSatellite::new(&name, &endpoint, true, None, None), json);
    }

    if !json {
        outln!("Enrolling satellite {ssh_host}...");
    }

    let outcome = crate::commands::enroll::enroll_over_ssh(
        ssh_host,
        endpoint_override,
        quic_port,
        install_service,
        &mut |event| match event {
            crate::commands::enroll::EnrollEvent::ServiceInstalled { quic_bind } if !json => {
                outln!("  service installed on {ssh_host} (quic {quic_bind})");
            }
            crate::commands::enroll::EnrollEvent::ServiceInstalled { .. } => {}
            crate::commands::enroll::EnrollEvent::ServiceInstallFailed { error } => {
                eprintln!(
                    "phux satellite enroll: warning: could not install the remote service: {error}"
                );
            }
        },
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(crate::commands::enroll::EnrollFailure::MissingPhux(err)) => {
            if json {
                return crate::commands::json_err::emit(
                    true,
                    &crate::commands::json_err::CliError::new(
                        crate::commands::json_err::codes::REGISTRY,
                        format!("satellite enroll: {err}"),
                        format!(
                            "install phux on {ssh_host} first, or use \
                             `phux satellite enroll {ssh_host} --ssh-only`"
                        ),
                    ),
                    1,
                );
            }
            eprintln!("phux satellite enroll: {err}");
            eprintln!(
                "      Install phux on {ssh_host} first, or use \
                 `phux satellite enroll {ssh_host} --ssh-only`."
            );
            return ExitCode::FAILURE;
        }
        Err(crate::commands::enroll::EnrollFailure::Pair(err)) => {
            return enroll_failure(json, &err);
        }
    };
    let crate::commands::enroll::EnrollOutcome { endpoint, report } = outcome;
    let token_file = (!endpoint.starts_with("ssh://")).then(|| {
        crate::commands::enroll::satellite_token_path(&phux_server::telemetry::state_dir(), &name)
    });
    let new = match NewSatellite::new(
        &name,
        &endpoint,
        true,
        token_file.as_deref(),
        report.cert_fingerprint.as_deref(),
    ) {
        Ok(new) => new,
        Err(err) => return enroll_failure(json, &err),
    };
    if let Some(path) = &token_file
        && let Err(err) = crate::commands::enroll::write_token(path, &report.token)
    {
        return enroll_failure(json, &err);
    }

    finish_enroll(Ok(new), json)
}

fn finish_enroll(new: Result<NewSatellite, String>, json: bool) -> ExitCode {
    let result = run_add(new, json);
    if result == ExitCode::SUCCESS && !json {
        outln!("  Hub route ready. Ensure this host runs `phux service install --hub`.");
    }
    result
}

/// Report an enrollment failure: prose with the established prefix, or the
/// shared JSON error contract under `--json`.
fn enroll_failure(json: bool, message: &str) -> ExitCode {
    if json {
        return crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::REGISTRY,
                format!("satellite enroll: {message}"),
                "retry once the ssh path works, or register the route without \
                 probing via `phux satellite enroll HOST --ssh-only`",
            ),
            1,
        );
    }
    eprintln!("phux satellite enroll: {message}");
    ExitCode::FAILURE
}

fn run_list(json: bool) -> ExitCode {
    match load_registry() {
        Ok(entries) if json => print_satellites_json(&entries),
        Ok(entries) => {
            for entry in entries {
                outln!("{}", describe(&entry));
            }
            ExitCode::SUCCESS
        }
        Err(err) => fail(json, &err),
    }
}

fn run_add(new: Result<NewSatellite, String>, json: bool) -> ExitCode {
    let satellite = match new {
        Ok(satellite) => satellite,
        Err(err) => return fail(json, &err),
    };
    match add_or_update(&satellite) {
        Ok(entry) if json => print_satellite_json("satellite", &entry),
        Ok(entry) => {
            outln!("satellite {}", describe(&entry));
            ExitCode::SUCCESS
        }
        Err(err) => fail(json, &err),
    }
}

/// One human-readable line per entry. Auth material (ADR-0038) is shown by
/// reference only: the token-file *path* and the certificate fingerprint are
/// displayable, the token bytes never are (and are never read here).
fn describe(entry: &registry::SatelliteEntry) -> String {
    use std::fmt::Write as _;

    let state = if entry.enabled { "enabled" } else { "disabled" };
    let mut line = format!("{} {} ({state})", entry.name, entry.endpoint);
    if let Some(token_file) = &entry.token_file {
        let _ = write!(line, " token-file={}", token_file.display());
    }
    if let Some(fingerprint) = &entry.cert_fingerprint {
        let _ = write!(line, " cert-fingerprint={fingerprint}");
    }
    line
}

fn run_remove(name: &str, json: bool) -> ExitCode {
    let entry = match find_entry(name) {
        Ok(entry) => entry,
        Err(err) => return fail(json, &err),
    };
    match remove_entry(entry.index) {
        Ok(()) if json => print_satellite_json("removed", &entry),
        Ok(()) => {
            outln!("removed {}", entry.name);
            ExitCode::SUCCESS
        }
        Err(err) => fail(json, &err),
    }
}

/// Report a satellite-registry failure: the historical prose line without
/// `--json` (byte-identical, so scripts that grep stderr keep working), or
/// one line of the shared JSON error contract with it (phux-i0e8.8.3).
fn fail(json: bool, message: &str) -> ExitCode {
    if json {
        return crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::REGISTRY,
                message,
                "run `phux satellite list` to see configured satellites; \
                 `phux config path` names the config file",
            ),
            1,
        );
    }
    eprintln!("phux: {message}");
    ExitCode::FAILURE
}
