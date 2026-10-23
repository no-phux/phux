//! Resolve organization targets and project their typed refusals.
use super::{
    Connection, ExitCode, KeyMatch, KeyNames, OrgError, OrgFilter, OrgInventory, Refusal, Selector,
    ServerTarget, Target, codes, org, selector,
};

impl Refusal {
    /// An organization request that failed: refusals exit 2, everything
    /// else 1.
    pub(super) fn org(verb: &str, err: &OrgError) -> Self {
        let exit = match err {
            OrgError::Refused(_) | OrgError::Server { .. } | OrgError::Ambiguous { .. } => 2,
            _ => 1,
        };
        let remedy = match err {
            OrgError::Unsupported => "upgrade the server (`phux upgrade`)",
            OrgError::OutcomeUnknown { .. } => {
                "inspect authoritative state with phux ls; keep the original operation id and do not blindly replay"
            }
            OrgError::NotFound(_) => "run `phux ls` to see sessions and panes",
            _ => "run `phux ls` to see the current state, then retry",
        };
        {
            let mut refusal = Self::new(err.code(), format!("{verb}: {err}"), remedy, exit);
            refusal.err = refusal.err.with_recovery(err.recovery());
            refusal
        }
    }
}

/// `^` and the shortest prefix of `key` no other record of `inventory`
/// shares ([`KeyNames`]).
pub(super) fn key_text(inventory: &OrgInventory, key: [u8; 16]) -> String {
    KeyNames::of_inventory(inventory).display(key)
}

/// Connect to `server`, reporting an unreachable server under `verb`.
pub(super) async fn connect(
    server: &ServerTarget,
    json: bool,
    verb: &str,
) -> Result<Connection, ExitCode> {
    server
        .connect()
        .await
        .map_err(|err| server.report_unreachable(json, &err, verb))
}

/// Read every record, refusing a server without `ORGANIZATION` first.
pub(super) async fn records(conn: &mut Connection, verb: &str) -> Result<OrgInventory, Refusal> {
    if !org::supported(conn) {
        return Err(Refusal::org(verb, &OrgError::Unsupported));
    }
    org::fetch(conn, OrgFilter::All, 1)
        .await
        .map_err(|err| Refusal::org(verb, &err))
}

/// Resolve `text` to one session or resource record (see the module docs).
pub(crate) async fn resolve(
    conn: &mut Connection,
    inventory: &OrgInventory,
    text: &str,
    verb: &str,
) -> Result<Target, Refusal> {
    let parsed = selector::parse(text).map_err(|err| {
        Refusal::new(
            codes::INVALID_SELECTOR,
            format!("invalid target '{text}': {err}"),
            "run `phux help targets` for the selector grammar",
            1,
        )
    })?;
    match &parsed {
        Selector::Key(prefix) => {
            return match inventory.resolve_key_prefix(prefix) {
                Ok(KeyMatch::Session(record)) => Ok(Target::Session(record)),
                Ok(KeyMatch::Resource(record)) => Ok(Target::Resource(record)),
                Err(err) => Err(Refusal::org(verb, &err)),
            };
        }
        Selector::Session(name) => {
            if let Some(record) = inventory.session_named(name) {
                return Ok(Target::Session(record.clone()));
            }
        }
        _ => {}
    }
    let (snapshot, _) = phux_client::state::get_state_on(conn)
        .await
        .map_err(|err| {
            Refusal::new(
                codes::TRANSPORT,
                format!("{verb}: could not read state: {err}"),
                "run `phux doctor` for a health check",
                1,
            )
        })?
        .into_parts();
    let ids = phux_client::state::resolve_targets_on(conn, &parsed, &snapshot)
        .await
        .map_err(|err| {
            Refusal::new(
                codes::NO_SUCH_TARGET,
                format!("{verb}: {err}"),
                "run `phux ls` to see sessions and panes",
                1,
            )
        })?;
    let [id] = ids.as_slice() else {
        if ids.is_empty() {
            return Err(Refusal::new(
                codes::NO_SUCH_TARGET,
                format!("no such target: {text}"),
                "run `phux ls` to see sessions and panes",
                1,
            ));
        }
        return Err(Refusal::new(
            codes::SELECTOR_NOT_SINGLE,
            format!("'{text}' names {} panes; {verb} takes one", ids.len()),
            "name one pane (`@N`, `name:N.M`, or a key `^ab23cd`)",
            1,
        ));
    };
    inventory
        .resource_by_live(id)
        .cloned()
        .map(Target::Resource)
        .ok_or_else(|| {
            Refusal::new(
                codes::NO_SUCH_TARGET,
                format!("{text} has no record on this server"),
                "run `phux ls` to see sessions and panes",
                1,
            )
        })
}
