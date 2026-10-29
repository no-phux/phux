//! Choosing the new session's name: the requested one, or a fresh unique
//! random one.

use std::path::Path;
use std::process::ExitCode;

use crate::commands::server_target::ServerTarget;
use crate::commands::{
    attach::configured_session_name_template, attach::render_default_session_name, partial,
};

/// How many generated names an omitted-name `phux new` tries before falling
/// back to a numeric suffix.
pub(super) const RANDOM_NAME_ATTEMPTS: usize = 8;

/// The session name from the positional NAME or `-s`; a conflict is
/// rejected.
pub(super) fn requested_session_name(
    name: Option<String>,
    session: Option<String>,
) -> Result<Option<String>, ExitCode> {
    match (name, session) {
        (Some(positional), Some(flag)) if positional != flag => {
            error!(
                "conflicting session names: '{positional}' (positional) vs '{flag}' (-s) — pass just one"
            );
            Err(ExitCode::FAILURE)
        }
        (Some(positional), _) => Ok(Some(positional)),
        (None, flag) => Ok(flag),
    }
}

/// Refuse a name no selector could reach again (`phux attach x:y` reads a
/// window of `x`): exit 2, before any server is dialed or spawned.
pub(super) fn report_invalid_session_name(
    json: bool,
    name: &str,
    invalid: &phux_client::rename::SessionNameError,
) -> ExitCode {
    use crate::commands::json_err::{CliError, codes, emit};
    emit(
        json,
        &CliError::new(
            codes::INVALID_SESSION_NAME,
            format!("invalid session name {name:?}: {invalid}"),
            "pick a non-empty name of at most 256 bytes, without control characters, a leading \
             `@`, `#`, or `%`, `:`, or `/@`",
        ),
        2,
    )
}

/// The session names already on the server, for duplicate rejection and
/// auto-suffixing. No server (or no answer) means none.
pub(super) fn existing_session_names(
    rt: &tokio::runtime::Runtime,
    target: &ServerTarget,
) -> Vec<String> {
    if target.socket_path().is_some_and(|path| !path.exists()) {
        return Vec::new();
    }
    rt.block_on(target.get_state()).map_or_else(
        |_| Vec::new(),
        // Session names are hub-local, so an unreachable satellite cannot hide a
        // duplicate: warn, don't refuse.
        |view| {
            partial::warn_partial_view("new", view.degradation());
            view.snapshot()
                .sessions
                .iter()
                .map(|session| session.name.clone())
                .collect()
        },
    )
}

/// The name to create: the requested one unless it is taken, else the
/// configured template made unique.
pub(super) fn choose_session_name(
    requested: Option<String>,
    existing: &[String],
) -> Result<String, ExitCode> {
    let Some(requested) = requested else {
        // No name given: render the session-name-template and disambiguate.
        // `${cwd-basename}` uses the local directory even for a remote server.
        return Ok(fresh_session_name(
            existing,
            &configured_session_name_template(),
            &std::env::current_dir().unwrap_or_default(),
            &mut phux_config::NameRng::from_entropy(),
        ));
    };
    if existing.contains(&requested) {
        error!("session '{requested}' already exists (use `phux attach {requested}` to join it)");
        return Err(ExitCode::FAILURE);
    }
    Ok(requested)
}

/// A session name for an omitted-name `phux new` not in `existing`: render
/// once, redraw a taken `${random-name}` up to [`RANDOM_NAME_ATTEMPTS`] times,
/// else suffix the first render via [`unique_session_name`].
pub(super) fn fresh_session_name(
    existing: &[String],
    template: &str,
    cwd: &Path,
    rng: &mut phux_config::NameRng,
) -> String {
    let first = render_default_session_name(template, cwd, rng);
    if !is_taken(existing, &first) {
        return first;
    }
    if phux_config::template_has_random_name(template)
        && let Some(retry) = retry_random_name(existing, template, cwd, rng)
    {
        return retry;
    }
    unique_session_name(existing, &first)
}

/// Re-render a `${random-name}` template until a pick is free, spending the
/// attempts left after the first render.
pub(super) fn retry_random_name(
    existing: &[String],
    template: &str,
    cwd: &Path,
    rng: &mut phux_config::NameRng,
) -> Option<String> {
    (1..RANDOM_NAME_ATTEMPTS)
        .map(|_| render_default_session_name(template, cwd, rng))
        .find(|candidate| !is_taken(existing, candidate))
}

pub(super) fn is_taken(existing: &[String], name: &str) -> bool {
    existing.iter().any(|e| e == name)
}

/// `base` if free, else the first free `base-2`, `base-3`, ... A suffixed
/// name shortens `base` so it stays within the session-name byte limit.
pub(crate) fn unique_session_name(existing: &[String], base: &str) -> String {
    if !existing.iter().any(|e| e == base) {
        return base.to_owned();
    }
    let mut n: u32 = 2;
    loop {
        let suffix = format!("-{n}");
        let mut end = base
            .len()
            .min(phux_client::rename::MAX_SESSION_NAME_BYTES - suffix.len());
        while !base.is_char_boundary(end) {
            end -= 1;
        }
        let candidate = format!("{}{suffix}", &base[..end]);
        if !existing.iter().any(|e| e == &candidate) {
            return candidate;
        }
        n = n.saturating_add(1);
    }
}
