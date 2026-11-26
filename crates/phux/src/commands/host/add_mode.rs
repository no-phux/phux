//! Which of `phux host add`'s forms an invocation is (manual, ssh, or a
//! connect link), decided from the arguments alone.

use phux_client_runtime::pairing::{is_connect_link, parse_connect_link};

use super::super::enroll;
use super::super::json_err::{CliError, codes};
use super::super::remote_target::{endpoint_host, endpoint_port, registration_name};

use super::HostRole;
use super::args::AddOpts;
/// Which of `host add`'s two forms an invocation is, decided from the
/// arguments alone so the rule is one place and testable.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AddMode {
    /// Register `endpoint` under `name` from what was typed.
    Manual { name: String, endpoint: String },
    /// Set `target` up over ssh.
    Ssh { target: String },
    /// Register `name` from a connect link: `--code LINK` (`-` for stdin),
    /// or a link pasted where the endpoint goes.
    Code { name: String, link: String },
}

impl AddMode {
    /// `NAME ENDPOINT` and a bare endpoint URI are the manual form; anything
    /// else is an ssh destination. A flag from the other form is refused.
    pub(super) fn classify(
        target: &str,
        endpoint: Option<&str>,
        opts: &AddOpts,
    ) -> Result<Self, CliError> {
        if let Some(linked) = Self::link_form(target, endpoint, opts) {
            return linked;
        }
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
                None => endpoint_host(target)
                    .map(|host| registration_name(opts.role, &host, endpoint_port(target)))
                    .ok_or_else(|| {
                        CliError::new(
                            codes::REGISTRY,
                            format!(
                                "could not derive a name from {:?}",
                                phux_client_runtime::pairing::redact_uri(target)
                            ),
                            "pass --name NAME, or `phux host add NAME ENDPOINT`",
                        )
                    })?,
            };
            Some((name, target.to_owned()))
        } else {
            None
        };

        let Some((name, endpoint)) = manual else {
            if manual_credentials_given(opts) {
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

    /// The link form, when the invocation is one: `--code LINK`, a connect
    /// link pasted as ENDPOINT, or one pasted as TARGET alone.
    fn link_form(
        target: &str,
        endpoint: Option<&str>,
        opts: &AddOpts,
    ) -> Option<Result<Self, CliError>> {
        if opts.code.is_some() {
            return Some(Self::code(target, endpoint, opts));
        }
        match endpoint {
            Some(link) if is_connect_link(link) => Some(Self::from_link(
                target.to_owned(),
                link.to_owned(),
                opts,
                opts.name.is_some(),
            )),
            None if is_connect_link(target) => Some(Self::linked_target(target, opts)),
            _ => None,
        }
    }

    /// `NAME --code LINK`: the link carries the endpoint and every
    /// credential, so nothing that names them too is taken.
    fn code(target: &str, endpoint: Option<&str>, opts: &AddOpts) -> Result<Self, CliError> {
        if endpoint.is_some() {
            return Err(code_conflict("an ENDPOINT"));
        }
        let link = opts.code.clone().unwrap_or_default();
        Self::from_link(target.to_owned(), link, opts, opts.name.is_some())
    }

    /// A connect link pasted as TARGET alone: registered under `--name`, else
    /// the name the link lists the machine under.
    fn linked_target(link: &str, opts: &AddOpts) -> Result<Self, CliError> {
        let parsed = parse_connect_link(link).map_err(|err| {
            CliError::new(
                codes::REGISTRY,
                format!("the connect link does not parse: {err}"),
                "mint a new link with `phux pair` on that machine, then run \
                 `phux host add NAME --code LINK`",
            )
        })?;
        let name = opts.name.clone().or(parsed.name).ok_or_else(|| {
            CliError::new(
                codes::REGISTRY,
                "the connect link names no machine to register it under",
                "pass the name first: `phux host add NAME --code LINK`",
            )
        })?;
        Self::from_link(name, link.to_owned(), opts, false)
    }

    /// The link form, refused beside any flag that names what the link
    /// carries. `name_flagged`: `--name` competes with a NAME already given.
    fn from_link(
        name: String,
        link: String,
        opts: &AddOpts,
        name_flagged: bool,
    ) -> Result<Self, CliError> {
        let conflict = if opts.role != HostRole::Remote {
            Some("--role satellite")
        } else if name_flagged {
            Some("--name")
        } else if opts.session.is_some() {
            Some("--session")
        } else if manual_credentials_given(opts) {
            Some("a manual credential flag")
        } else {
            ssh_form_flag_given(opts)
        };
        conflict.map_or(Ok(Self::Code { name, link }), |conflict| {
            Err(code_conflict(conflict))
        })
    }
}

/// A flag beside a connect link that names what the link already carries.
fn code_conflict(conflict: &str) -> CliError {
    CliError::new(
        codes::REGISTRY,
        format!(
            "a connect link (`--code`) does not combine with {conflict}: the link carries the endpoint and its credentials"
        ),
        "pass only the name to register and the link: `phux host add NAME --code LINK`",
    )
}

/// Whether a flag only the manual form takes names a credential or route.
const fn manual_credentials_given(opts: &AddOpts) -> bool {
    opts.token_file.is_some()
        || opts.cert_fingerprint.is_some()
        || opts.tls_server_name.is_some()
        || opts.ca_fingerprint.is_some()
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
