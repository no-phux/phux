//! `phux host add`: running the chosen form, the manual (`NAME ENDPOINT`) path,
//! and the error shapes every add path shares.

use std::process::ExitCode;

use super::super::enroll::{CertificateStatus, EnrollFailure};
use super::super::json_err::{self, CliError, codes};
use super::super::remote::{self};
use super::super::satellite as satellite_registry;
use super::super::service;

use super::HostRole;
use super::add_code::run_add_from_code;
pub(super) use super::add_mode::AddMode;
use super::add_ssh::run_add_over_ssh;
use super::args::AddOpts;
use super::enrollment::{EnrollmentReport, report_registered};
use super::list::HostRow;

/// Refuse a flag paired with the role it does not apply to, naming the
/// remedy. Post-parse: the parser cannot condition a flag on another's value.
pub(super) fn role_flag_mismatch(
    role: HostRole,
    has_session: bool,
    has_disabled: bool,
) -> Option<CliError> {
    match role {
        HostRole::Satellite if has_session => Some(CliError::new(
            codes::REGISTRY,
            "--session applies to --role remote only: a satellite link is \
             hub-dialed, so there is no arrival to attach",
            "drop --session, or register the machine as a remote \
             (`phux host add HOST --session NAME`)",
        )),
        HostRole::Remote if has_disabled => Some(CliError::new(
            codes::REGISTRY,
            "--disabled applies to --role satellite only: a remote entry has \
             no enabled bit",
            "drop --disabled, or register the machine with \
             `phux host add --role satellite HOST --disabled`",
        )),
        _ => None,
    }
}

/// `phux host add`.
pub(super) fn run_add(target: &str, endpoint: Option<&str>, opts: &AddOpts) -> ExitCode {
    let json = opts.json.json;
    if let Some(err) = role_flag_mismatch(opts.role, opts.session.is_some(), opts.disabled) {
        return json_err::emit(json, &err, 2);
    }
    if opts.role == HostRole::Satellite && opts.tls_server_name.is_some() {
        let err = CliError::new(
            codes::REGISTRY,
            "--tls-server-name applies to --role remote only: a satellite link is \
             hub-dialed, never through a relay route",
            "drop --tls-server-name, or register the machine as a remote",
        );
        return json_err::emit(json, &err, 2);
    }
    if let Err(err) = relay_route_authority(endpoint.unwrap_or(target), opts) {
        return json_err::emit(json, &err, 2);
    }
    let mode = AddMode::classify(target, endpoint, opts);
    if opts.hardware_key {
        if let Some(err) = hardware_key_misplaced(opts.role, mode.as_ref().ok()) {
            return json_err::emit(json, &err, 2);
        }
        if let Err(err) = hardware_key_ready(true) {
            return json_err::emit(json, &err, 1);
        }
    }
    match mode {
        Err(err) => json_err::emit(json, &err, 2),
        Ok(AddMode::Manual { name, endpoint }) => run_add_manual(&name, &endpoint, opts),
        Ok(AddMode::Ssh { target }) => run_add_over_ssh(&target, opts),
        Ok(AddMode::Code { name, link }) => run_add_from_code(&name, &link, opts),
    }
}

/// `--hardware-key` on a form that enrolls no client certificate (the manual
/// form, a satellite): refused instead of silently registering a file key.
pub(super) fn hardware_key_misplaced(role: HostRole, mode: Option<&AddMode>) -> Option<CliError> {
    (role != HostRole::Remote || matches!(mode, Some(AddMode::Manual { .. }))).then(|| {
        CliError::new(
            codes::REGISTRY,
            "--hardware-key applies where a client certificate is enrolled: \
             `phux host add HOST` over ssh, or `--code LINK`, for a remote",
            "drop --hardware-key",
        )
    })
}

/// `--hardware-key` checked before anything is contacted or written: this
/// machine's hardware makes a key (kept nowhere), or the command stops.
// ponytail: the check is one throwaway enclave key per command; the
// enrollment makes its own. Pass this one down if the extra key matters.
pub(super) fn hardware_key_ready(wanted: bool) -> Result<(), CliError> {
    if !wanted {
        return Ok(());
    }
    phux_client_runtime::hardware_key::mint()
        .map(drop)
        .map_err(|reason| {
            CliError::new(
                codes::WORKLOAD,
                format!("--hardware-key: {reason}"),
                "nothing was changed; drop --hardware-key to keep the key in a file",
            )
        })
}

/// The manual form: register exactly what was typed.
pub(super) fn run_add_manual(name: &str, endpoint: &str, opts: &AddOpts) -> ExitCode {
    let row = match opts.role {
        HostRole::Remote => remote::NewRemote::new(
            name,
            endpoint,
            opts.token_file.as_deref(),
            opts.cert_fingerprint.as_deref(),
            opts.session.as_deref(),
        )
        .and_then(|new| new.with_tls_server_name(opts.tls_server_name.as_deref()))
        .map_err(reject_entry)
        .and_then(|new| {
            remote::add_or_update(&new).map_err(registry_failure)?;
            // The server's CA, keyed by the relay's pin and the route: what a
            // relay link's `ca` records (ADR-0180).
            remote::pin_authority(
                &new.name,
                new.cert_fingerprint.as_deref(),
                new.tls_server_name.as_deref(),
                relay_route_authority(endpoint, opts)?.as_deref(),
            );
            Ok(HostRow::from_new_remote(new))
        }),
        HostRole::Satellite => satellite_registry::NewSatellite::new(
            name,
            endpoint,
            !opts.disabled,
            opts.token_file.as_deref(),
            opts.cert_fingerprint.as_deref(),
        )
        .map_err(reject_entry)
        .and_then(|new| {
            satellite_registry::add_or_update(&new)
                .map(HostRow::from_satellite)
                .map_err(registry_failure)
        }),
    };
    let row = match row {
        Ok(row) => row,
        Err(err) => return json_err::emit(opts.json.json, &err, 1),
    };
    let how = match (row.enabled, row.endpoint.starts_with("ssh://")) {
        (Some(false), _) => "disabled",
        (_, true) => "rides your ssh trust",
        (_, false) => "credentials as given",
    };
    report_registered(
        &row,
        how,
        &[],
        opts.json.json,
        None,
        &EnrollmentReport::new(CertificateStatus::Skipped),
    )
}

/// The server CA a manual relay-route registration pins (ADR-0180): required
/// with `--tls-server-name` on a `quic://` endpoint, since the relay would
/// otherwise read the session, and refused anywhere else.
pub(super) fn relay_route_authority(
    endpoint: &str,
    opts: &AddOpts,
) -> Result<Option<String>, CliError> {
    let relayed = opts.tls_server_name.is_some() && endpoint.starts_with("quic://");
    let Some(pin) = opts.ca_fingerprint.as_deref() else {
        if relayed {
            return Err(CliError::new(
                codes::REGISTRY,
                "a relay route needs the certificate authority of the server behind it: \
                 every dial runs end to end with that server, so the relay never reads it",
                "pass --ca-fingerprint (the `ca_fingerprint` `phux pair --relay-route` prints), \
                 or register from the relay link with `phux attach --remote NAME --code LINK`",
            ));
        }
        return Ok(None);
    };
    if !relayed || opts.role != HostRole::Remote {
        return Err(CliError::new(
            codes::REGISTRY,
            "--ca-fingerprint names the server behind a relay route",
            "pass it with --tls-server-name and the relay's quic:// endpoint (`--role remote`)",
        ));
    }
    phux_config::known_authorities::canonical_authority(pin)
        .map(Some)
        .ok_or_else(|| {
            CliError::new(
                codes::REGISTRY,
                format!("--ca-fingerprint {pin:?} is not a CA pin"),
                "pass sha256: and 64 hex digits, as `phux pair --relay-route` prints it",
            )
        })
}

/// A rejected field (bad endpoint scheme, missing pin, sigil name): the
/// registry's own message already names the fix.
pub(super) fn reject_entry(message: String) -> CliError {
    CliError::new(
        codes::REGISTRY,
        message,
        "fix the flagged field and rerun `phux host add`",
    )
}

/// A registry read/write failure underneath a validated request.
pub(super) fn registry_failure(message: String) -> CliError {
    CliError::new(
        codes::REGISTRY,
        message,
        "fix the reported [[remote]] / [[satellites]] entry; \
         `phux config path` names the config file",
    )
}

/// Map a failure from the shared ssh middle onto the CLI error contract,
/// with a remedy the operator can paste per failure class.
pub(super) fn add_failure_error(
    ssh_host: &str,
    role: HostRole,
    failure: &EnrollFailure,
) -> CliError {
    // A remedy the operator can paste verbatim: it must carry the role they
    // asked for, or following it would register into the wrong registry.
    let role_flag = role.flag();
    match failure {
        EnrollFailure::SshUnreachable(detail)
            if let Some(remedy) =
                phux_client_runtime::connection::host_key_remedy(detail, ssh_host) =>
        {
            CliError::new(
                codes::REGISTRY,
                format!("ssh refused {ssh_host}'s host key"),
                remedy,
            )
        }
        EnrollFailure::SshUnreachable(detail)
            if let Some(hint) =
                phux_client_runtime::connection::ssh_failure_hint(detail, ssh_host) =>
        {
            CliError::new(
                codes::REGISTRY,
                format!("cannot reach {ssh_host} over ssh: {detail}"),
                hint,
            )
        }
        EnrollFailure::SshUnreachable(detail) => {
            let check = format!(
                "check that `ssh {ssh_host}` works from this machine, then rerun \
                 `phux host add {ssh_host}{role_flag}`; or register it without contacting it: \
                 `phux host add {ssh_host}{role_flag} --ssh-only`"
            );
            // Nothing listening on the ssh port is the usual first-time
            // blocker on a Mac or a fresh tailnet machine.
            let remedy = if detail.contains("Connection refused") {
                format!(
                    "nothing accepts ssh there: on a Mac turn on Remote Login (System Settings > \
                     General > Sharing), on Linux start sshd, or on a tailnet run \
                     `tailscale set --ssh` there; {check}"
                )
            } else {
                check
            };
            CliError::new(
                codes::REGISTRY,
                format!("cannot reach {ssh_host} over ssh: {detail}"),
                remedy,
            )
        }
        EnrollFailure::MissingPhux(detail) => CliError::new(
            codes::REGISTRY,
            format!("{ssh_host} has no phux on the PATH its ssh shell sees: {detail}"),
            format!(
                "let `phux host add {ssh_host}{role_flag} --install` install it, or install it there \
                 yourself: `ssh {ssh_host} 'curl -fsSL https://phux.sh/install | sh'`; \
                 or, if it is installed where a non-interactive shell does not look, name it: \
                 `phux host add {ssh_host}{role_flag} --remote-phux ~/.local/bin/phux`"
            ),
        ),
        EnrollFailure::InstallFailed(detail) => CliError::new(
            codes::REGISTRY,
            format!("could not install phux on {ssh_host}: {detail}"),
            format!(
                "run the installer there to see all of its output: \
                 `ssh {ssh_host} 'curl -fsSL https://phux.sh/install | sh'` \
                 (releases exist for Apple silicon macOS and glibc Linux on x86_64 and arm64), \
                 then rerun `phux host add {ssh_host}{role_flag}`"
            ),
        ),
        EnrollFailure::Pair(detail) => CliError::new(
            codes::REGISTRY,
            detail.clone(),
            format!(
                "look at the host (`ssh {ssh_host} phux doctor`) and rerun \
                 `phux host add {ssh_host}{role_flag}`, or register an ssh-only entry: \
                 `phux host add {ssh_host}{role_flag} --ssh-only`"
            ),
        ),
        EnrollFailure::NotListening(detail) => CliError::new(
            codes::REGISTRY,
            format!("phux on {ssh_host} accepts no network connections: {detail}"),
            if detail.contains(service::PORT_TAKEN) {
                format!(
                    "pick a free port: `phux host add {ssh_host}{role_flag} --quic-port PORT`, \
                     or use the machine over ssh alone: `phux attach --ssh {ssh_host}`"
                )
            } else {
                format!(
                    "update phux there, which keeps its panes: \
                     `phux host add {ssh_host}{role_flag} --install`, \
                     or use the machine over ssh alone: `phux attach --ssh {ssh_host}`"
                )
            },
        ),
    }
}

/// A field the target registry rejected after enrollment (bad endpoint
/// scheme, missing pin, sigil name): the registry's message names the fix.
pub(super) fn reject_enrollment(message: String) -> CliError {
    CliError::new(
        codes::REGISTRY,
        message,
        "fix the flagged field and rerun `phux host add`",
    )
}
