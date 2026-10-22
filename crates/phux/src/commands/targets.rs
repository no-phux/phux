//! Selector parsing and the shared single-pane resolver behind every
//! verb that addresses one pane.

use std::path::Path;
use std::process::ExitCode;

use crate::commands::{json_err, partial};

/// Parse an optional target string into a [`phux_client::selector::Selector`],
/// defaulting to the focused session when absent. On a parse error,
/// prints a diagnostic and returns the failure exit code for the caller to
/// bubble.
pub(crate) fn parse_selector(
    session: Option<&str>,
) -> Result<phux_client::selector::Selector, ExitCode> {
    session.map_or(Ok(phux_client::selector::Selector::Current), |target| {
        phux_client::selector::parse(target).map_err(|err| {
            error!("invalid target '{target}': {err}");
            ExitCode::FAILURE
        })
    })
}

/// Resolve `selector` to a single pane against a fresh `GET_STATE`
/// snapshot. Prefers the focused pane when the selector spans several
/// (e.g. a whole session); otherwise the first in snapshot order. Prints
/// diagnostics and returns the failure exit code on no-server / miss.
///
/// This is the shared front door for every verb that addresses one pane
/// (`snapshot`, `send-keys`, `paste`, `run`, `wait`, `watch`, `resize`,
/// `signal`, `rec`, `ask`), so it is also where the partial-fleet distinction
/// is drawn for all of them: a miss against a hub that could not reach a
/// satellite is reported as unresolvable, never as absent.
///
/// It uses [`partial::report_target_miss_keeping_status`] rather than the
/// distinct exit status `kill`/`tag`/`agent` return, because two of the verbs
/// behind this door have already spent their status space — `run` mirrors the
/// child's own exit code and `wait` owns `124`. A shared resolver cannot hand
/// out a code that means one thing for `kill` and collides for `run`, so the
/// distinction stays in the sentence, which is where the user reads it.
///
/// `json` selects the failure channel per the JSON error contract
/// ([`json_err`], phux-i0e8.8.2): verbs without a `--json` flag pass `false`
/// and keep the historical prose.
pub(crate) async fn resolve_target(
    socket_path: &Path,
    selector: &phux_client::selector::Selector,
    verb: &str,
    json: bool,
) -> Result<phux_protocol::ids::ResourceId, ExitCode> {
    resolve_target_with(socket_path, selector, verb, json, false).await
}

/// [`resolve_target`] for the verbs that deliver input into the pane
/// (`send-keys`, `paste`, `run`, `signal`, and the acknowledged agent
/// writes, `agent start` among them): identical, except that a `%name` whose record has the withdrawn
/// shape is refused (ADR-0075 point 5) rather than resolved.
pub(crate) async fn resolve_target_for_input(
    socket_path: &Path,
    selector: &phux_client::selector::Selector,
    verb: &str,
    json: bool,
) -> Result<phux_protocol::ids::ResourceId, ExitCode> {
    resolve_target_with(socket_path, selector, verb, json, true).await
}

async fn resolve_target_with(
    socket_path: &Path,
    selector: &phux_client::selector::Selector,
    verb: &str,
    json: bool,
    for_input: bool,
) -> Result<phux_protocol::ids::ResourceId, ExitCode> {
    let (snapshot, degradation) = phux_client::state::get_state(socket_path)
        .await
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, verb))?
        .into_parts();
    // `%name` is singular: it resolves to exactly one agent or refuses, and
    // never travels the set-valued path below where `pick_target_pane` would
    // narrow it (ADR-0075 point 3). A Terminal-facet verb acts on the named
    // agent's pane; the session verbs resolve their own side.
    if let phux_client::selector::Selector::Agent(name) = selector {
        let target =
            phux_client::state::resolve_agent_target(socket_path, name, &snapshot, for_input)
                .await
                .map_err(|err| report_agent_resolve_error(json, &err, true))?;
        partial::warn_partial_view(verb, &degradation);
        return Ok(target.terminal);
    }
    let candidates = resolve_targets(socket_path, selector, &snapshot)
        .await
        .map_err(|err| report_agent_resolve_error(json, &err, true))?;
    let picked = phux_client::selector::pick_target_pane(&candidates, &snapshot.focused_resource)
        .ok_or_else(|| {
        partial::report_target_miss_keeping_status_for(
            json,
            Some(&selector.to_string()),
            &degradation,
        )
    })?;
    // A hit is still worth a word: the pane we picked is the best of what a
    // partial fleet offered, and the user is about to act on it.
    partial::warn_partial_view(verb, &degradation);
    Ok(picked)
}

/// Report a `%name` refusal on the shared error contract and return its
/// exit status.
///
/// Every variant is a refusal to guess (ADR-0075 point 3), so each lands on
/// a distinct code: a miss is `no_such_target` (1); two records or two
/// sessions sharing the name are `selector_not_single` (2); a kind constant
/// is `invalid_agent_name` (2); a withdrawn record on an input verb is
/// `agent_withdrawn` (2); an index that did not finish is `partial_view` —
/// exit 3, or 1 when `keep_status` for the verbs whose status is already
/// spoken for.
pub(crate) fn report_agent_resolve_error(
    json: bool,
    err: &phux_client::selector::AgentResolveError,
    keep_status: bool,
) -> ExitCode {
    json_err::emit(
        json,
        &json_err::CliError::new(agent_resolve_code(err), err.to_string(), err.remedy()),
        agent_resolve_exit(err, keep_status),
    )
}

/// The CLI vocabulary's spelling of [`AgentResolveError::code`], so the
/// codes stay in [`json_err::codes`] (a test pins the two equal).
///
/// [`AgentResolveError::code`]: phux_client::selector::AgentResolveError::code
pub(super) const fn agent_resolve_code(
    err: &phux_client::selector::AgentResolveError,
) -> &'static str {
    use phux_client::selector::AgentResolveError;
    match err {
        AgentResolveError::Unknown { .. } => json_err::codes::NO_SUCH_TARGET,
        AgentResolveError::Ambiguous { .. } | AgentResolveError::AmbiguousSession { .. } => {
            json_err::codes::SELECTOR_NOT_SINGLE
        }
        AgentResolveError::KindConstant { .. } => json_err::codes::INVALID_AGENT_NAME,
        AgentResolveError::Withdrawn { .. } => json_err::codes::AGENT_WITHDRAWN,
        AgentResolveError::PartialIndex { .. } => json_err::codes::PARTIAL_VIEW,
    }
}

/// [`AgentResolveError::exit_code`], except a partial index exits 1 under
/// `keep_status` (the shared single-pane resolver, ADR-0075 point 3).
///
/// [`AgentResolveError::exit_code`]: phux_client::selector::AgentResolveError::exit_code
pub(super) const fn agent_resolve_exit(
    err: &phux_client::selector::AgentResolveError,
    keep_status: bool,
) -> u8 {
    if keep_status
        && matches!(
            err,
            phux_client::selector::AgentResolveError::PartialIndex { .. }
        )
    {
        return crate::exit_codes::EXIT_FAILURE;
    }
    err.exit_code()
}

/// Resolve `selector` to its `ResourceId`s, fetching L3 metadata only for
/// `#tag` (`phux-f8wi`) and `%name` (ADR-0075). Other selectors resolve
/// purely against `snapshot`, so the common path pays no extra round-trip.
///
/// A tag fetch that fails (no server mid-flight, a malformed value) degrades
/// to an empty tag index, so a `#tag` selector then resolves to nothing —
/// the caller reports it as a selector miss, never a hang. A `%name` resolves
/// to its one Terminal or a typed refusal the caller reports with
/// [`report_agent_resolve_error`].
pub(crate) async fn resolve_targets(
    socket_path: &Path,
    selector: &phux_client::selector::Selector,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
) -> Result<Vec<phux_protocol::ids::ResourceId>, phux_client::selector::AgentResolveError> {
    phux_client::state::resolve_targets(socket_path, selector, snapshot).await
}
