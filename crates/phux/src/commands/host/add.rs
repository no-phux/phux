//! `phux host add`: choosing the form, the manual (`NAME ENDPOINT`) path,
//! and the error shapes every add path shares.

use std::process::ExitCode;

use super::super::enroll::{self, CertificateStatus, EnrollFailure};
use super::super::json_err::{self, CliError, codes};
use super::super::remote::{self};
use super::super::remote_target::endpoint_host;
use super::super::satellite as satellite_registry;

use super::HostRole;
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

/// Which of `host add`'s two forms an invocation is, decided from the
/// arguments alone so the rule is one place and testable.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AddMode {
    /// Register `endpoint` under `name` from what was typed.
    Manual { name: String, endpoint: String },
    /// Set `target` up over ssh.
    Ssh { target: String },
}

impl AddMode {
    /// `NAME ENDPOINT` and a bare endpoint URI are the manual form; anything
    /// else is an ssh destination. A flag from the other form is refused.
    pub(super) fn classify(
        target: &str,
        endpoint: Option<&str>,
        opts: &AddOpts,
    ) -> Result<Self, CliError> {
        let manual = if let Some(endpoint) = endpoint {
            if opts.name.is_some() {
                return Err(CliError::new(
                    codes::REGISTRY,
                    "--name does not combine with a NAME ENDPOINT pair: the first argument is the name",
                    "drop --name, or pass only the endpoint URI with --name NAME",
                ));
            }
            Some((target.to_owned(), endpoint.to_owned()))
        } else if target.contains("://") {
            let name = match &opts.name {
                Some(name) => name.clone(),
                None => endpoint_host(target).ok_or_else(|| {
                    CliError::new(
                        codes::REGISTRY,
                        format!("could not derive a name from {target:?}"),
                        "pass --name NAME, or `phux host add NAME ENDPOINT`",
                    )
                })?,
            };
            Some((name, target.to_owned()))
        } else {
            None
        };

        let Some((name, endpoint)) = manual else {
            if opts.token_file.is_some()
                || opts.cert_fingerprint.is_some()
                || opts.tls_server_name.is_some()
                || opts.ca_fingerprint.is_some()
            {
                return Err(CliError::new(
                    codes::REGISTRY,
                    "--token-file, --cert-fingerprint, --tls-server-name, and --ca-fingerprint belong to the manual form, which registers an endpoint you already hold credentials for",
                    format!(
                        "pass the endpoint too (`phux host add {target} quic://HOST:PORT --token-file PATH --cert-fingerprint FP`), \
                         or drop them and let `phux host add {target}` pair over ssh"
                    ),
                ));
            }
            return Ok(Self::Ssh {
                target: target.to_owned(),
            });
        };
        if let Some(flag) = ssh_form_flag_given(opts) {
            return Err(CliError::new(
                codes::REGISTRY,
                format!(
                    "{flag} belongs to the ssh form (`phux host add [USER@]HOST`), which sets the machine up itself"
                ),
                format!(
                    "drop {flag}, or drop the endpoint URI and let `phux host add HOST` pair over ssh"
                ),
            ));
        }
        Ok(Self::Manual { name, endpoint })
    }
}

/// The first ssh-form flag present on a manual invocation, by its spelling.
pub(super) fn ssh_form_flag_given(opts: &AddOpts) -> Option<&'static str> {
    if opts.ssh_only {
        Some("--ssh-only")
    } else if opts.endpoint_override.is_some() {
        Some("--endpoint")
    } else if opts.no_service {
        Some("--no-service")
    } else if opts.remote_phux != "phux" {
        Some("--remote-phux")
    } else if opts.install {
        Some("--install")
    } else if opts.accept_new_host_key {
        Some("--accept-new-host-key")
    } else if opts.quic_port != enroll::default_quic_port() {
        Some("--quic-port")
    } else {
        None
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
    match AddMode::classify(target, endpoint, opts) {
        Err(err) => json_err::emit(json, &err, 2),
        Ok(AddMode::Manual { name, endpoint }) => run_add_manual(&name, &endpoint, opts),
        Ok(AddMode::Ssh { target }) => run_add_over_ssh(&target, opts),
    }
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
