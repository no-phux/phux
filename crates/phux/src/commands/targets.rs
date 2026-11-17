//! Selector parsing and the shared single-pane resolver behind every
//! verb that addresses one pane.

use std::path::Path;
use std::process::ExitCode;

use phux_client::read::Verb;
use phux_client::selector::{AgentResolveError, Resolved, Selector};
use phux_protocol::ids::ResourceId;

use crate::commands::json_err::{Refusal, codes};
use crate::commands::{partial, read_kind};

/// Parse an optional target string into a [`Selector`], defaulting to the
/// focused session when absent. On a parse error, prints a diagnostic and
/// returns the failure exit code for the caller to bubble.
pub(crate) fn parse_selector(session: Option<&str>) -> Result<Selector, ExitCode> {
    parse_target(session).map_err(|refusal| refusal.emit(false))
}

/// [`parse_selector`] as a refusal: `invalid_selector`, exit 1. A human
/// reads the one line [`parse_selector`] has always printed.
pub(crate) fn parse_target(session: Option<&str>) -> Result<Selector, Refusal> {
    let Some(target) = session else {
        return Ok(Selector::Current);
    };
    phux_client::selector::parse(target).map_err(|err| {
        let message = format!("invalid target '{target}': {err}");
        Refusal::new(
            codes::INVALID_SELECTOR,
            message.clone(),
            "selector forms: NAME, NAME:WIN, NAME:WIN.PANE, @ID, `.`, #TAG, %NAME",
            crate::exit_codes::EXIT_FAILURE,
        )
        .with_human(vec![message])
    })
}

/// Resolve `selector` to a single pane against a fresh `GET_STATE`
/// snapshot. Prefers the focused pane when the selector spans several
/// (e.g. a whole session); otherwise the first in snapshot order. Refuses
/// on no-server / miss.
///
/// This is the shared front door for every verb that addresses one pane
/// (`snapshot`, `send-keys`, `paste`, `run`, `wait`, `watch`, `resize`,
/// `signal`, `rec`, `ask`), so it is also where the partial-fleet distinction
/// is drawn for all of them: a miss against a hub that could not reach a
/// satellite is reported as unresolvable, never as absent.
///
/// It uses [`partial::target_miss_keeping_status`] rather than the distinct
/// exit status `kill`/`tag`/`agent` return, because two of the verbs behind
/// this door have already spent their status space — `run` mirrors the
/// child's own exit code and `wait` owns `124`. A shared resolver cannot hand
/// out a code that means one thing for `kill` and collides for `run`, so the
/// distinction stays in the sentence, which is where the user reads it.
///
/// Every refusal follows the JSON error contract ([`crate::commands::json_err`],
/// phux-i0e8.8.2) on whichever channel the verb reports it.
pub(crate) async fn resolve_target(
    socket_path: &Path,
    selector: &Selector,
    verb: &str,
) -> Result<ResourceId, Refusal> {
    resolve_target_with(socket_path, selector, verb, false)
        .await
        .map(|resolved| resolved.id)
}

/// [`resolve_target`], reporting the resolved resource's kind too, for the
/// verbs that dispatch on it ([`phux_client::read`]); `for_input` as
/// [`resolve_target_for_input`].
pub(crate) async fn resolve_resource(
    socket_path: &Path,
    selector: &Selector,
    verb: &str,
    for_input: bool,
) -> Result<Resolved, Refusal> {
    resolve_target_with(socket_path, selector, verb, for_input).await
}

/// [`resolve_terminal_target`] for the verbs that deliver input into the
/// pane (`send-keys`, `paste`, `run`, `take`, `give`, and the acknowledged
/// agent writes, `agent start` among them): identical, except that a
/// `%name` whose record has the withdrawn shape is refused (ADR-0075 point
/// 5) rather than resolved.
pub(crate) async fn resolve_target_for_input(
    socket_path: &Path,
    selector: &Selector,
    verb: Verb,
) -> Result<ResourceId, Refusal> {
    resolve_terminal_with(socket_path, selector, verb, true).await
}

/// [`resolve_target`] for a verb that reaches a Terminal only (`resize`,
/// `ask`, `rec`): a resource of another kind is refused before anything is
/// sent, `wrong_resource_kind` and exit 2, as `send-keys` refuses it. The
/// kind is read from the snapshot the resolution already fetched.
pub(crate) async fn resolve_terminal_target(
    socket_path: &Path,
    selector: &Selector,
    verb: Verb,
) -> Result<ResourceId, Refusal> {
    resolve_terminal_with(socket_path, selector, verb, false).await
}

async fn resolve_terminal_with(
    socket_path: &Path,
    selector: &Selector,
    verb: Verb,
    for_input: bool,
) -> Result<ResourceId, Refusal> {
    let resolved = resolve_target_with(socket_path, selector, verb.name(), for_input).await?;
    phux_client::read::require_terminal(verb, &resolved)
        .map_err(|err| read_kind::wrong_kind_refusal(&err))?;
    Ok(resolved.id)
}

async fn resolve_target_with(
    socket_path: &Path,
    selector: &Selector,
    verb: &str,
    for_input: bool,
) -> Result<Resolved, Refusal> {
    let (snapshot, degradation) = phux_client::state::get_state(socket_path)
        .await
        .map_err(|err| Refusal::no_server(&err, socket_path, verb))?
        .into_parts();
    // `%name` is singular: it resolves to exactly one agent or refuses, and
    // never travels the set-valued path below where `pick_target_pane` would
    // narrow it (ADR-0075 point 3). A Terminal-facet verb acts on the named
    // agent's pane; the session verbs resolve their own side.
    if let Selector::Agent(name) = selector {
        let target =
            phux_client::state::resolve_agent_target(socket_path, name, &snapshot, for_input)
                .await
                .map_err(|err| agent_refusal(&err, true))?;
        partial::warn_partial_view(verb, &degradation);
        return Ok(Resolved::in_snapshot(&snapshot, target.terminal));
    }
    let candidates = resolve_targets(socket_path, selector, &snapshot)
        .await
        .map_err(|err| agent_refusal(&err, true))?;
    let picked = phux_client::selector::pick_target_pane(&candidates, &snapshot.focused_resource)
        .ok_or_else(|| {
        partial::target_miss_keeping_status(Some(&selector.to_string()), &degradation)
    })?;
    // A hit is still worth a word: the pane we picked is the best of what a
    // partial fleet offered, and the user is about to act on it.
    partial::warn_partial_view(verb, &degradation);
    Ok(Resolved::in_snapshot(&snapshot, picked))
}

/// Report a `%name` refusal on the shared error contract and return its
/// exit status.
pub(crate) fn report_agent_resolve_error(
    json: bool,
    err: &AgentResolveError,
    keep_status: bool,
) -> ExitCode {
    agent_refusal(err, keep_status).emit(json)
}

/// The refusal behind [`report_agent_resolve_error`].
pub(crate) fn agent_refusal(err: &AgentResolveError, keep_status: bool) -> Refusal {
    Refusal::new(
        err.code(),
        err.to_string(),
        err.remedy(),
        agent_resolve_exit(err, keep_status),
    )
}

/// [`AgentResolveError::exit_code`], except a partial index exits 1 under
/// `keep_status` (the shared single-pane resolver, ADR-0075 point 3).
pub(super) const fn agent_resolve_exit(err: &AgentResolveError, keep_status: bool) -> u8 {
    if keep_status && matches!(err, AgentResolveError::PartialIndex { .. }) {
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
    selector: &Selector,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
) -> Result<Vec<ResourceId>, AgentResolveError> {
    phux_client::state::resolve_targets(socket_path, selector, snapshot).await
}
