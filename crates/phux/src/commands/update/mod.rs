//! `phux update` — the one-command path from the release phux is running to
//! the release phux publishes. The compatibility unit is the release
//! (ADR-0071: the wire keeps its own `0.x` line), so moving a fleet between
//! releases has to be one command.
//!
//! Trust boundary:
//!
//! * The artifact's `.sha256` sidecar must carry a minisign signature from a
//!   key compiled into this binary (see [`signature`], ADR-0178); a missing,
//!   invalid, or foreign signature installs nothing.
//! * The download's digest must equal that signed digest, and the channel
//!   manifest's when it names the build, before anything is unpacked; a
//!   mismatch installs nothing.
//! * Nothing downloaded is executed to decide whether to install it; the
//!   archive is data, validated before and after unpacking.
//! * Replacement is atomic and permission-preserving (see [`apply`]).
//! * An install phux does not own (Homebrew, Cargo, Nix) gets its native
//!   command instead, and an unrecognized install is refused.
//!
//! `phux upgrade` remains the low-level primitive that asks a running server to
//! re-exec the binary on disk; `phux update` puts a new binary there and then
//! calls it so live panes survive.

pub(crate) mod apply;
pub(crate) mod channel;
mod model;
mod outcome;
pub(crate) mod release;
pub(crate) mod signature;
pub(crate) mod source;

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client_runtime::release::{
    self as shared, Asset, Build, Channel, Entry, Product, Version,
};
use usage::Args;

use self::channel::ChannelArg;
use self::release::ReleaseSource;
use self::source::{Install, InstallSource, UnknownReason};
use super::json_err::{self, CliError, codes};
use crate::exit_codes::{EXIT_FAILURE, EXIT_SUCCESS, EXIT_USAGE};

pub(crate) use model::*;
pub(crate) use outcome::*;

/// Run one `phux update` against injected effects, so the whole flow is
/// testable with a fake [`ReleaseSource`] and a scratch bin directory.
pub(crate) fn execute(opts: &UpdateOpts, env: &UpdateEnv<'_>) -> Result<Outcome, UpdateError> {
    let install = env.install.clone();
    let current = release::current_version()?;
    if opts.rollback {
        return rollback(opts, install, current, env);
    }

    let host_target = release::host_target()?;
    let pin = opts.tag.as_deref().map(release::validate_tag).transpose()?;
    let channel = channel::resolve(opts.channel.map(Channel::from), &install);
    if channel == Channel::Next && pin.is_some() {
        return Err(UpdateError::InvalidTag(
            "`--version` pins a stable tag; omit it when using `--channel next`".to_owned(),
        ));
    }
    let installed = channel::installed(&install, current);
    let published = env
        .releases
        .manifest(channel)
        .and_then(|body| release::published(&body, channel, host_target));
    let plan = plan(
        install,
        current,
        &installed,
        channel,
        published,
        pin,
        host_target,
    )?;

    let action = if opts.check {
        Action::Checked
    } else if plan.changes_version {
        refuse_unpinned_downgrade(&plan, pin.is_some())?;
        return install_release(opts, env, plan);
    } else {
        Action::UpToDate
    };
    Ok(Outcome {
        action,
        plan,
        artifact: None,
        digest: None,
        binaries: Vec::new(),
        backup: None,
        handoff: None,
    })
}

/// Only a `--version` pin moves a stable install to an older release. The
/// manifest is unsigned, so a host that rewrites it could otherwise name an
/// older, genuinely signed release with a known flaw and roll every
/// `phux update` back to it (ADR-0178).
fn refuse_unpinned_downgrade(plan: &Plan, pinned: bool) -> Result<(), UpdateError> {
    if pinned || plan.update_available {
        return Ok(());
    }
    Err(UpdateError::Downgrade(format!(
        "the {} release manifest names {}, which is older than the installed {}; \
         refusing to downgrade without a `--version` pin",
        plan.channel.as_str(),
        plan.target.tag(),
        plan.current
    )))
}

/// Download, verify, and (unless `--dry-run`) replace.
fn install_release(
    opts: &UpdateOpts,
    env: &UpdateEnv<'_>,
    plan: Plan,
) -> Result<Outcome, UpdateError> {
    let bin_dir = plan
        .install
        .bin_dir()
        .ok_or_else(|| {
            UpdateError::Install(format!(
                "{} has no parent directory to install into",
                plan.install.executable.display()
            ))
        })?
        .to_path_buf();
    let artifact = shared::asset(Product::Phux, &plan.target, plan.host_target);
    let staging = apply::Staging::create(&bin_dir)?;

    // Downloads land in staging, removed on every exit path.
    let archive_path = staging.path().join(&artifact.name);
    env.releases.download(&artifact.url, &archive_path)?;

    // The gate: nothing below runs unless a release key signed the digest
    // and the bytes match it.
    let expected = signed_digest(env, &artifact, staging.path())?;
    if let Some(entry) = plan.vouched()
        && entry.sha256 != expected
    {
        return Err(UpdateError::Signature(format!(
            "the {} release manifest publishes {} for {}, but the signed checksum is {expected}",
            plan.channel.as_str(),
            entry.sha256,
            artifact.name
        )));
    }
    let digest = apply::verify_digest(&archive_path, &expected, &artifact.name)?;
    let staged = apply::unpack_verified(&archive_path, &artifact.stem, staging.path())?;

    if opts.dry_run {
        let would_replace: Vec<String> = apply::RELEASE_BINARIES
            .iter()
            .filter(|name| **name == "phux" || bin_dir.join(name).exists())
            .map(|name| (*name).to_owned())
            .collect();
        return Ok(Outcome {
            action: Action::Planned,
            plan,
            artifact: Some(artifact),
            digest: Some(digest),
            binaries: would_replace,
            backup: None,
            handoff: None,
        });
    }

    let replaced = apply::replace_binaries(&bin_dir, &staged, &plan.current.to_string())?;
    channel::persist(&bin_dir, plan.channel)?;
    let handoff = if opts.no_restart {
        Handoff::Skipped
    } else {
        (env.handoff)()
    };

    Ok(Outcome {
        action: Action::Installed,
        plan,
        artifact: Some(artifact),
        digest: Some(digest),
        binaries: replaced.binaries,
        backup: Some(replaced.backup),
        handoff: Some(handoff),
    })
}

/// Download `artifact`'s `.sha256` sidecar and its `.minisig` into `staging`
/// and return the digest a release key signed. A signature that cannot be
/// fetched refuses like one that does not verify: an unsigned release is
/// never installed.
fn signed_digest(
    env: &UpdateEnv<'_>,
    artifact: &Asset,
    staging: &std::path::Path,
) -> Result<String, UpdateError> {
    let sidecar_path = staging.join(format!("{}.sha256", artifact.name));
    env.releases
        .download(&artifact.sidecar_url(), &sidecar_path)?;
    let signature_path = staging.join(format!("{}.sha256.minisig", artifact.name));
    env.releases
        .download(&artifact.signature_url(), &signature_path)
        .map_err(|err| {
            UpdateError::Signature(format!(
                "no signature for {} could be fetched ({err}); an unsigned release is \
                 never installed",
                artifact.name
            ))
        })?;
    let read = |path: &std::path::Path| {
        std::fs::read(path).map_err(|err| {
            UpdateError::Checksum(format!("could not read {}: {err}", path.display()))
        })
    };
    signature::signed_digest(
        &read(&sidecar_path)?,
        &read(&signature_path)?,
        &artifact.name,
        env.release_keys,
    )
}

/// Restore the binaries saved by the previous update. The handoff runs here
/// too, so the running server goes back to the previous image;
/// `--no-restart` suppresses it as on the way forward.
fn rollback(
    opts: &UpdateOpts,
    install: Install,
    current: Version,
    env: &UpdateEnv<'_>,
) -> Result<Outcome, UpdateError> {
    let bin_dir = install
        .bin_dir()
        .ok_or_else(|| {
            UpdateError::NoBackup(format!(
                "{} has no parent directory to look for a backup in",
                install.executable.display()
            ))
        })?
        .to_path_buf();
    let restored = apply::rollback(&bin_dir)?;
    let target = Version::parse(&restored.version).unwrap_or(current);
    let plan = Plan {
        install,
        current,
        channel: Channel::Stable,
        current_sha: None,
        latest: None,
        target: Build::Stable(target),
        changes_version: target != current,
        update_available: false,
        host_target: release::host_target().unwrap_or("unknown"),
    };
    let handoff = if opts.no_restart {
        Handoff::Skipped
    } else {
        (env.handoff)()
    };
    Ok(Outcome {
        action: Action::RolledBack,
        plan,
        artifact: None,
        digest: None,
        binaries: restored.binaries,
        backup: None,
        handoff: Some(handoff),
    })
}

/// `phux update` — check for, download, verify, and install a newer phux,
/// then hand a running server off to it.
pub(crate) fn run_update(opts: &UpdateOpts, socket: Option<PathBuf>) -> ExitCode {
    let probe = match source::Probe::from_process() {
        Ok(probe) => probe,
        Err(err) => {
            return json_err::emit(
                opts.json,
                &CliError::new(
                    codes::INTERNAL_ERROR,
                    format!("could not resolve this binary's own path: {err}"),
                    "phux update needs to know which file it would replace".to_owned(),
                ),
                EXIT_FAILURE,
            );
        }
    };
    let install = source::detect(&probe);
    let (socket_path, explicit_socket) = super::upgrade::resolve_socket(socket);

    // Refuse a mutation of an install phux does not own before any network
    // traffic; `--check` may always report.
    if !opts.check
        && let Some(refusal) = Refusal::of(install.source)
    {
        let err = CliError::new(
            refusal.code(),
            refusal.message(&install),
            Refusal::remedy(&install),
        );
        return json_err::emit(opts.json, &err, EXIT_USAGE);
    }

    // Machine output never prompts and keeps stderr quiet; the flag still
    // grants the agent-session recovery.
    let consent = if opts.json && !opts.close_agent_sessions {
        super::upgrade::Consent::Advise
    } else {
        super::upgrade::Consent::detect(opts.close_agent_sessions)
    };
    let handoff = || live_handoff(&socket_path, explicit_socket.as_deref(), consent, opts.json);
    let env = UpdateEnv {
        releases: &release::NetworkReleaseSource,
        handoff: &handoff,
        install,
        release_keys: signature::RELEASE_KEYS,
    };

    match execute(opts, &env) {
        Ok(outcome) => {
            let shadow = std::env::var_os("PATH")
                .and_then(|path| shadowing_phux(&outcome.plan.install, &path));
            if let Err(err) = print_outcome(&outcome, shadow.as_deref(), opts.json) {
                return json_err::emit(
                    true,
                    &CliError::new(
                        codes::JSON_SERIALIZE,
                        format!("could not render the update document: {err}"),
                        "report this at https://github.com/no-phux/phux/issues".to_owned(),
                    ),
                    EXIT_FAILURE,
                );
            }
            reconcile_installed(outcome.action, opts.json, |print| {
                super::service::reconcile_after_update(print);
            });
            ExitCode::from(EXIT_SUCCESS)
        }
        Err(err) => err.report(opts.json),
    }
}

/// Print the update result and any stale binary shadowing it on PATH.
fn print_outcome(
    outcome: &Outcome,
    shadow: Option<&std::path::Path>,
    json: bool,
) -> Result<(), serde_json::Error> {
    if json {
        let mut document = outcome.document();
        document["path_shadowed_by"] = shadow.map(|path| path.display().to_string()).into();
        outln!("{}", serde_json::to_string_pretty(&document)?);
        return Ok(());
    }
    for line in outcome.lines() {
        outln!("{line}");
    }
    if let Some(path) = shadow {
        outln!("{}", shadow_warning(path, &outcome.plan.install.executable));
    }
    Ok(())
}

/// The first `phux` on `PATH`, when it is not the binary this command
/// maintains. A stale `cargo install` or version-manager shim ahead of the
/// release install makes every update look like it did nothing.
fn shadowing_phux(install: &Install, path: &std::ffi::OsStr) -> Option<PathBuf> {
    if install.source != InstallSource::DirectRelease {
        return None;
    }
    let ours = std::fs::canonicalize(&install.executable).ok()?;
    let first = std::env::split_paths(path)
        .map(|dir| dir.join("phux"))
        .find(|candidate| crate::companion::is_executable(candidate))?;
    let resolved = std::fs::canonicalize(&first).ok()?;
    (resolved != ours).then_some(first)
}

fn shadow_warning(shadow: &std::path::Path, ours: &std::path::Path) -> String {
    format!(
        "warning:  `phux` on your PATH is {} first, not {}\n  \
         remove the stale copy or put {} earlier on PATH (`which -a phux` lists them)",
        shadow.display(),
        ours.display(),
        ours.parent().unwrap_or(ours).display()
    )
}

/// Reconcile every successful install while keeping machine output silent.
fn reconcile_installed(action: Action, json: bool, reconcile: impl FnOnce(bool)) {
    if action == Action::Installed {
        reconcile(!json);
    }
}

/// Ask the running server to re-exec, through `phux upgrade`'s own driver:
/// transient refusals are retried, and an older server blocked by agent
/// sessions gets them closed under `consent`. `socket` is the explicit
/// `--socket`, echoed into any printed command; `quiet` keeps stderr clean
/// for `--json`.
fn live_handoff(
    socket_path: &std::path::Path,
    socket: Option<&std::path::Path>,
    consent: super::upgrade::Consent,
    quiet: bool,
) -> Handoff {
    use phux_client::attach::AttachError;

    let outcome = super::upgrade::LiveUpgrade::new(socket_path, quiet)
        .and_then(|mut env| super::upgrade::drive(&mut env, consent, socket));
    match outcome {
        Ok(super::upgrade::UpgradeAck::Upgrading) => Handoff::Upgrading,
        Ok(
            super::upgrade::UpgradeAck::Refused(message)
            | super::upgrade::UpgradeAck::Unexpected(message),
        ) => Handoff::Refused(message),
        Err(AttachError::Io(err))
            if matches!(
                err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            Handoff::NoServer
        }
        Err(err) => Handoff::Failed(err.to_string()),
    }
}

#[cfg(test)]
mod tests;
