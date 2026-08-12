//! Re-export of the client-side selector grammar (phux-3kj, ADR-0021).
//!
//! The grammar and its resolution now live in
//! [`phux_client::selector`] so the MCP adapter (phux-gj6) can reach the
//! same parser and resolver the CLI uses, rather than carrying a reduced
//! duplicate. This module re-exports it under the `selector::` path the
//! binary already references throughout `lib.rs`.

//! One form does **not** come through this door. `%name`
//! ([ADR-0075](../../../ADR/0075-agent-name-addressing.md)) parses to
//! [`Selector::Agent`] like every other spelling, but it resolves to exactly
//! one Terminal *or refuses* — so it must not travel the set-valued
//! [`resolve`] / [`pick_target_pane`] path the rest of this module re-exports.
//! [`phux_client::selector::resolve_with_tags`] deliberately yields nothing
//! for it, which makes an un-migrated caller fail closed with a selector miss.
//! Verbs that mean to accept `%name` branch on [`Selector::Agent`] first and
//! call `phux_client::selector::resolve_agent` (read-only verbs) or
//! `resolve_agent_for_input` (`send-keys`, `paste`, `signal`, `run`, and the
//! ADR-0053 batch verbs, which additionally refuse the withdrawn shape),
//! mapping `AgentResolveError::exit_code` onto the verb's status.

pub(crate) use phux_client::selector::{
    Selector, format_terminal_id, parse, pick_target_pane, resolve, resolve_with_tags,
    whole_session_name,
};

// `WindowRef` is re-exported for the binary's tests (parse-grammar
// assertions); the non-test build references only `Selector`/`parse`/
// `resolve`, so gate it to avoid an unused-import warning there.
#[cfg(test)]
pub(crate) use phux_client::selector::WindowRef;
