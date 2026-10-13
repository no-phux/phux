//! Where a new session, tab, or split starts, and how a picker groups sessions.

use phux_client_core::organization::{self, Place};

/// Focused host and directory, unless an explicit non-blank field overrides it.
#[must_use]
pub fn place_for(focused: Option<&Place>, explicit: Option<&Place>) -> Place {
    match explicit {
        None => organization::place_for(focused, organization::PlaceSource::Focused),
        Some(explicit) => organization::place_for(
            focused,
            organization::PlaceSource::Explicit {
                host: explicit.host.clone(),
                directory: explicit.directory.clone(),
            },
        ),
    }
}

/// Host embedded in `satellite:<host>:<id>`. Local terminals have none.
#[must_use]
pub fn terminal_host(terminal_id: Option<&str>) -> Option<String> {
    let terminal_id = terminal_id?;
    let rest = terminal_id.strip_prefix("satellite:")?;
    let split = rest.rfind(':')?;
    let host = &rest[..split];
    (!host.is_empty()).then(|| host.to_owned())
}

/// One session row a picker can group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLine {
    /// Session name.
    pub name: String,
    /// Remote host. Absent is the attached server.
    pub host: Option<String>,
    /// Project tag.
    pub project: Option<String>,
}

/// Sessions that share a project tag and host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGroup {
    /// Absent is the untagged group, which sorts last.
    pub project: Option<String>,
    /// Remote host, when the group is remote.
    pub host: Option<String>,
    /// Members, in input order.
    pub sessions: Vec<SessionLine>,
}

/// Group by project tag, then remote host. Untagged and local sort last.
#[must_use]
pub fn group_by_project(sessions: &[SessionLine]) -> Vec<ProjectGroup> {
    let mut keys = Vec::new();
    for session in sessions {
        let key = (blank(&session.project), blank(&session.host));
        if !keys.iter().any(|item| item == &key) {
            keys.push(key);
        }
    }
    keys.sort_by(|left, right| compare_key(&left.0, &right.0).then(compare_key(&left.1, &right.1)));
    keys.into_iter()
        .filter_map(|(project, host)| {
            let members: Vec<SessionLine> = sessions
                .iter()
                .filter(|session| blank(&session.project) == project && blank(&session.host) == host)
                .cloned()
                .collect();
            if members.is_empty() {
                return None;
            }
            Some(ProjectGroup {
                project,
                host,
                sessions: members,
            })
        })
        .collect()
}

fn blank(value: &Option<String>) -> Option<String> {
    value.clone().filter(|text| !text.is_empty())
}

fn compare_key(left: &Option<String>, right: &Option<String>) -> std::cmp::Ordering {
    match (left, right) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(left), Some(right)) => left.cmp(right),
    }
}
