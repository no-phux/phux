//! Fleet level reads and inventory-only filtering.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_server::runtime::default_socket_path;

use super::action_flags::ListFlags;
use super::model::AgentStateReport;
use super::{AgentView, classify_snapshot, configured_agents, fetch_snapshot, print_agent_states};
use crate::commands::{cli_runtime, json_err, partial};

impl ListFlags {
    fn matches(&self, report: &AgentStateReport) -> bool {
        let state_matches = self.state.is_empty()
            || self
                .state
                .iter()
                .any(|state| state == report.state.as_str());
        state_matches && report.matches_query(self.query.as_deref().unwrap_or_default())
    }
}

pub(super) fn run(flags: &ListFlags, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let (snapshot, degradation) = match fetch_snapshot(&socket_path, "agent list").await {
            Ok(pair) => pair,
            Err(err) => {
                return json_err::report_no_server(flags.json, &err, &socket_path, "agent list");
            }
        };
        let plugins = configured_agents();
        let mut states = classify_snapshot(&socket_path, &snapshot, &plugins).await;
        // Addressability is computed against the whole inventory before filtering:
        // hiding a duplicate name must never turn it into a usable `%name`.
        states.retain(|report| flags.matches(report));
        // An enumeration, like `phux ls`: warn about a partial view and succeed.
        partial::warn_partial_view("agent list", &degradation);
        if states.is_empty() && (!flags.state.is_empty() || flags.query.is_some()) && !flags.json {
            outln!("No panes match the agent filters.");
            return ExitCode::SUCCESS;
        }
        print_agent_states(&states, flags.json, AgentView::Show)
    })
}

#[cfg(test)]
mod tests {
    use phux_client::agent_meta::{AgentMetaState, AgentRecord};

    use super::ListFlags;
    use crate::commands::agent::detect::infer_agent_state;
    use crate::commands::agent::model::PaneEvidence;

    #[test]
    fn list_filters_combine_states_with_cross_field_query_words() {
        let mut evidence = PaneEvidence::for_test("mini/@7", Some("Fix flaky tests"), &[]);
        evidence.cwd = Some("/src/backend".to_owned());
        evidence.record = Some(AgentRecord {
            name: "reviewer".to_owned(),
            kind: Some("claude".to_owned()),
            state: AgentMetaState::Blocked,
            ..AgentRecord::default()
        });
        let report = infer_agent_state(&evidence, &[]);
        let flags = ListFlags {
            state: vec!["blocked".to_owned(), "done".to_owned()],
            query: Some("  BACKEND\tClaude mini ".to_owned()),
            ..ListFlags::default()
        };
        assert!(flags.matches(&report));
        assert!(ListFlags::default().matches(&report));
        assert!(
            !ListFlags {
                state: vec!["working".to_owned()],
                ..flags
            }
            .matches(&report)
        );
        assert!(!report.matches_query("backend missing"));
        assert!(report.matches_query("tests reviewer"));
        assert!(report.matches_query(" \n\t "));
    }

    #[test]
    fn list_queries_do_not_search_screen_contents_or_provenance() {
        let evidence = PaneEvidence::for_test("@7", None, &["private transcript needle"]);
        let mut report = infer_agent_state(&evidence, &[]);
        assert!(!report.matches_query("private"));
        report.explanation = "private explanation".to_owned();
        assert!(!report.matches_query("private"));
        report.address = Some("%builder".to_owned());
        assert!(report.matches_query("%builder"));
        report.title = Some("Réparer".to_owned());
        assert!(report.matches_query("RÉPARER"));
    }
}
