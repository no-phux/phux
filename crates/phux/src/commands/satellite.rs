mod json;
mod registry;

use std::path::{Path, PathBuf};
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
        println!("Enrolling satellite {ssh_host}...");
    }

    if let Err(err) = crate::commands::enroll::remote_phux_version(ssh_host) {
        eprintln!("phux satellite enroll: {err}");
        eprintln!(
            "      Install phux on {ssh_host} first, or use \
             `phux satellite enroll {ssh_host} --ssh-only`."
        );
        return ExitCode::FAILURE;
    }

    if install_service {
        let quic_bind = format!("0.0.0.0:{quic_port}");
        match crate::commands::enroll::ssh_capture(
            ssh_host,
            &["phux", "service", "install", "--quic", &quic_bind],
        ) {
            Ok(_) if !json => {
                println!("  service installed on {ssh_host} (quic {quic_bind})");
            }
            Ok(_) => {}
            Err(err) => {
                eprintln!(
                    "phux satellite enroll: warning: could not install the remote service: {err}"
                );
            }
        }
    }

    let stdout = match crate::commands::enroll::ssh_capture(ssh_host, &["phux", "pair", "--json"]) {
        Ok(stdout) => stdout,
        Err(err) => return enroll_failure(&err),
    };
    let report = match crate::commands::enroll::PairReport::parse(&stdout) {
        Ok(report) => report,
        Err(err) => return enroll_failure(&err),
    };
    let endpoint =
        crate::commands::enroll::choose_endpoint(ssh_host, &report, endpoint_override, quic_port);
    let token_file = (!endpoint.starts_with("ssh://"))
        .then(|| satellite_token_path(&phux_server::telemetry::state_dir(), &name));
    let new = match NewSatellite::new(
        &name,
        &endpoint,
        true,
        token_file.as_deref(),
        report.cert_fingerprint.as_deref(),
    ) {
        Ok(new) => new,
        Err(err) => return enroll_failure(&err),
    };
    if let Some(path) = &token_file
        && let Err(err) = crate::commands::enroll::write_token(path, &report.token)
    {
        return enroll_failure(&err);
    }

    finish_enroll(Ok(new), json)
}

fn finish_enroll(new: Result<NewSatellite, String>, json: bool) -> ExitCode {
    let result = run_add(new, json);
    if result == ExitCode::SUCCESS && !json {
        println!("  Hub route ready. Ensure this host runs `phux service install --hub`.");
    }
    result
}

fn enroll_failure(message: &str) -> ExitCode {
    eprintln!("phux satellite enroll: {message}");
    ExitCode::FAILURE
}

fn satellite_token_path(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join("satellites").join(format!("{name}.token"))
}

fn run_list(json: bool) -> ExitCode {
    match load_registry() {
        Ok(entries) if json => print_satellites_json(&entries),
        Ok(entries) => {
            for entry in entries {
                println!("{}", describe(&entry));
            }
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err),
    }
}

fn run_add(new: Result<NewSatellite, String>, json: bool) -> ExitCode {
    let satellite = match new {
        Ok(satellite) => satellite,
        Err(err) => return fail(&err),
    };
    match add_or_update(&satellite) {
        Ok(entry) if json => print_satellite_json("satellite", &entry),
        Ok(entry) => {
            println!("satellite {}", describe(&entry));
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err),
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
        Err(err) => return fail(&err),
    };
    match remove_entry(entry.index) {
        Ok(()) if json => print_satellite_json("removed", &entry),
        Ok(()) => {
            println!("removed {}", entry.name);
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("phux: {message}");
    ExitCode::FAILURE
}
