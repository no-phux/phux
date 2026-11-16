//! Fleet level reads and inventory-only filtering.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_config::socket::default_socket_path;

use phux_client::agent::reads::{self, ListFilter};

use super::action_flags::ListFlags;
use super::{AgentView, print_agent_states, report_read_error, warn_partial_records};
use crate::commands::{cli_runtime, partial};

impl ListFlags {
    fn filter(&self) -> ListFilter<'_> {
        ListFilter {
            states: self.state.as_slice(),
            query: self.query.as_deref(),
        }
    }
}

pub(super) fn run(flags: &ListFlags, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let mut notices = Vec::new();
        let result = reads::list(&socket_path, &flags.filter(), &mut notices).await;
        warn_partial_records(&notices);
        let reads::Projection {
            reports: states,
            degradation,
        } = match result {
            Ok(projection) => projection,
            Err(err) => {
                return report_read_error(flags.json, &err, None, &socket_path, "agent list");
            }
        };
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

    use phux_client::agent::detect::{PaneEvidence, infer_agent_state};

    use super::ListFlags;

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
        assert!(flags.filter().matches(&report));
        assert!(ListFlags::default().filter().matches(&report));
        assert!(
            !ListFlags {
                state: vec!["working".to_owned()],
                ..flags
            }
            .filter()
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
