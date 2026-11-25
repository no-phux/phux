//! The `[policy] mode` a unit with a remote entry point needs (ADR-0179).

use std::path::Path;

use super::ServicePlan;

/// The listener-address variables a supervised server may inherit from the
/// environment the install runs in (a systemd user manager can import it).
const LISTENER_ENV: [&str; 3] = ["PHUX_WS_ADDR", "PHUX_QUIC_ADDR", "PHUX_WT_ADDR"];

/// Whether the installing environment names a listener address the unit's
/// server could inherit.
fn inherited_listener_env() -> bool {
    LISTENER_ENV
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// [`ensure_paired_policy`] against the active `config.toml` and the
/// installing environment; whether it wrote the mode.
pub(super) fn settle_policy(plan: &ServicePlan) -> Result<bool, String> {
    ensure_paired_policy(
        plan,
        &phux_config::loader::config_path(),
        inherited_listener_env(),
    )
}

/// The remote entry point the supervised server will open, if any: the
/// unit's own listener, a listener address it inherits, or a configured
/// `[[connector]]` (the server refuses each with no mode, ADR-0179).
fn remote_entry_point(
    plan: &ServicePlan,
    config: &phux_config::Config,
    inherited_env: bool,
) -> Option<&'static str> {
    if plan.quic.is_some() || plan.listen.is_some() {
        return Some("--quic/--listen");
    }
    if inherited_env {
        return Some("PHUX_WS_ADDR/PHUX_QUIC_ADDR/PHUX_WT_ADDR");
    }
    (!config.connector.is_empty()).then_some("[[connector]]")
}

/// Write `[policy] mode = "paired"` into `config`, creating the file and
/// the `[policy]` table when they are missing. Existing keys and comments
/// stay (`phux_config::settings::write_edit`).
pub(crate) fn write_paired_mode(config: &Path) -> Result<(), String> {
    phux_config::settings::write_edit(
        config,
        "policy.mode",
        phux_config::settings::Edit::Set(toml::Value::String("paired".to_owned())),
    )
    .map_err(|err| format!("could not set [policy] mode in {}: {err}", config.display()))?;
    // A paired server refuses to start on a state directory others can write.
    let state = phux_config::instance::state_dir();
    phux_server::owner_dir::tighten(&state)
        .map_err(|err| format!("could not make {} owner-only: {err}", state.display()))
}

/// A unit whose server opens a remote entry point starts only under
/// `[policy] mode = "paired"` (ADR-0179). With no mode in `config` that is
/// what the operator asked for by naming one, so write it, and say so;
/// `local` contradicts it and is refused, leaving the config alone. Nothing
/// changes for a unit with no remote entry point. Whether it wrote the mode.
pub(super) fn ensure_paired_policy(
    plan: &ServicePlan,
    config: &Path,
    inherited_env: bool,
) -> Result<bool, String> {
    let loaded = phux_config::loader::load_from(config)
        .map_err(|err| format!("{}: {err}; run `phux config check`", config.display()));
    let direct = plan.quic.is_some() || plan.listen.is_some() || inherited_env;
    let loaded = match loaded {
        Ok(loaded) => loaded,
        Err(err) if direct => return Err(err),
        // No listener of its own and an unreadable config: no connector is
        // known to settle, and the server reports the config itself.
        Err(_) => return Ok(false),
    };
    let Some(entry) = remote_entry_point(plan, &loaded, inherited_env) else {
        return Ok(false);
    };
    match loaded.policy.mode {
        Some(phux_config::PolicyMode::Paired) => Ok(false),
        Some(phux_config::PolicyMode::Local) => Err(format!(
            "{} sets [policy] mode = \"local\", which admits no remote entry point, but the \
             service would open one ({entry}); set it to \"paired\" (enrolled client \
             certificates only) or install without it",
            config.display()
        )),
        None => {
            write_paired_mode(config)?;
            info!(
                "service: set [policy] mode = \"paired\" in {} for {entry}: the remote entry \
                 point serves only clients holding an enrolled certificate (`phux host add`, \
                 `phux pair --enroll`)",
                config.display()
            );
            Ok(true)
        }
    }
}
