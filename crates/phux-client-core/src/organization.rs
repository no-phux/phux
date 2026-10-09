//! Where a new session, tab, or split starts, and how pickers group sessions.
//!
//! A spawn takes the focused pane's host and directory, or an explicit pick.
//! It does not read the client process's working directory and it does not
//! substitute a home directory.

/// Host and directory a new session, tab, or split should use.
///
/// `host` `None` is the server this client is attached to. `directory` `None`
/// means the server's own default, not a path invented on the client.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Place {
    /// Satellite or machine name. `None` is the attached server.
    pub host: Option<String>,
    /// Working directory on that host.
    pub directory: Option<String>,
}

/// Whether the spawn follows the focused pane or an explicit pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceSource {
    /// Copy the focused pane. No pane yields an empty [`Place`].
    Focused,
    /// A set field replaces the focused pane. An empty string inherits.
    Explicit {
        /// Explicit host, when the user picked one.
        host: Option<String>,
        /// Explicit directory, when the user picked one.
        directory: Option<String>,
    },
}

/// The host and directory a spawn should use.
#[must_use]
pub fn place_for(focused: Option<&Place>, source: PlaceSource) -> Place {
    let focused_host = focused.and_then(|place| blank_to_none(place.host.as_deref()));
    let focused_directory = focused.and_then(|place| blank_to_none(place.directory.as_deref()));
    match source {
        PlaceSource::Focused => Place {
            host: focused_host,
            directory: focused_directory,
        },
        PlaceSource::Explicit { host, directory } => Place {
            host: blank_to_none(host.as_deref()).or(focused_host),
            directory: blank_to_none(directory.as_deref()).or(focused_directory),
        },
    }
}

fn blank_to_none(value: Option<&str>) -> Option<String> {
    value.filter(|text| !text.is_empty()).map(ToOwned::to_owned)
}

/// One session row a picker can group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLine {
    /// Session name.
    pub name: String,
    /// Host the session lives on. `None` is the attached server.
    pub host: Option<String>,
    /// Shared project tag. `None` and `""` are untagged.
    pub project: Option<String>,
}

/// Sessions that share one project tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGroup {
    /// `None` is the untagged group, which sorts last.
    pub project: Option<String>,
    /// Member sessions, in input order.
    pub sessions: Vec<SessionLine>,
}

/// Group `sessions` by project tag. Tags sort ascending. Untagged is last.
/// Order inside a group is the input order.
#[must_use]
pub fn group_by_project(sessions: &[SessionLine]) -> Vec<ProjectGroup> {
    let mut tags: Vec<Option<String>> = Vec::new();
    for session in sessions {
        let tag = blank_to_none(session.project.as_deref());
        if tags.iter().all(|existing| existing != &tag) {
            tags.push(tag);
        }
    }
    tags.sort_by(|left, right| match (left, right) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(left), Some(right)) => left.cmp(right),
    });
    tags.into_iter()
        .map(|tag| ProjectGroup {
            sessions: sessions
                .iter()
                .filter(|session| blank_to_none(session.project.as_deref()) == tag)
                .cloned()
                .collect(),
            project: tag,
        })
        .filter(|group| !group.sessions.is_empty())
        .collect()
}

/// The session `step` away from `current` in `names`, wrapping.
/// `step` `1` is next and `-1` is previous. One name, or an empty list,
/// returns `None`.
#[must_use]
pub fn adjacent_name<'a>(names: &'a [&'a str], current: &str, step: i32) -> Option<&'a str> {
    if names.len() < 2 || step == 0 {
        return None;
    }
    let index = names.iter().position(|name| *name == current)?;
    let len = i32::try_from(names.len()).ok()?;
    let next = (i32::try_from(index).ok()? + step).rem_euclid(len);
    let next = usize::try_from(next).ok()?;
    names.get(next).copied()
}

/// The latest history entry that is non-empty and not `current`.
/// `history` is oldest first.
#[must_use]
pub fn last_name<'a>(history: &'a [&'a str], current: &str) -> Option<&'a str> {
    history
        .iter()
        .rev()
        .copied()
        .find(|name| !name.is_empty() && *name != current)
}

/// One session as a workspace archive records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizedSession {
    /// Session name.
    pub name: String,
    /// Host. `None` prints `local`.
    pub host: Option<String>,
    /// Directory. `None` prints `-`.
    pub directory: Option<String>,
    /// Project tag. `None` prints `-`.
    pub project: Option<String>,
}

/// Stable lines for a workspace archive, sorted by session name.
#[must_use]
pub fn organization_lines(sessions: &[OrganizedSession]) -> Vec<String> {
    let mut rows = sessions.to_vec();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows.into_iter()
        .map(|session| {
            let host = session.host.as_deref().filter(|host| !host.is_empty());
            let directory = session
                .directory
                .as_deref()
                .filter(|directory| !directory.is_empty());
            let project = session
                .project
                .as_deref()
                .filter(|project| !project.is_empty());
            format!(
                "{} host={} directory={} project={}",
                session.name,
                host.unwrap_or("local"),
                directory.unwrap_or("-"),
                project.unwrap_or("-"),
            )
        })
        .collect()
}

/// JSON body for `phux.session.create/v1`.
///
/// A directory starts a seed pane there and does not set `empty`. No
/// directory creates an empty keep-empty session, which is today's create.
#[must_use]
pub fn session_create_document(name: &str, directory: Option<&str>) -> String {
    directory
        .filter(|directory| !directory.is_empty())
        .map_or_else(
            || {
                format!(
                    "{{\"empty\":true,\"keep_empty\":true,\"name\":{}}}",
                    json_string(name),
                )
            },
            |directory| {
                format!(
                    "{{\"cwd\":{},\"keep_empty\":true,\"name\":{}}}",
                    json_string(directory),
                    json_string(name),
                )
            },
        )
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::{
        OrganizedSession, Place, PlaceSource, SessionLine, adjacent_name, group_by_project,
        last_name, organization_lines, place_for, session_create_document,
    };

    fn focused() -> Place {
        Place {
            host: Some("edge".to_owned()),
            directory: Some("/src/api".to_owned()),
        }
    }

    #[test]
    fn place_for_uses_the_focused_host_and_directory() {
        let place = place_for(Some(&focused()), PlaceSource::Focused);
        assert_eq!(place, focused());
    }

    #[test]
    fn place_for_does_not_invent_a_directory_without_focus() {
        let place = place_for(None, PlaceSource::Focused);
        assert_eq!(place, Place::default());
    }

    #[test]
    fn explicit_pick_overrides_and_blank_fields_inherit() {
        let place = place_for(
            Some(&focused()),
            PlaceSource::Explicit {
                host: Some(String::new()),
                directory: Some("/other".to_owned()),
            },
        );
        assert_eq!(
            place,
            Place {
                host: Some("edge".to_owned()),
                directory: Some("/other".to_owned()),
            }
        );
    }

    #[test]
    fn group_by_project_sorts_tags_and_keeps_untagged_last() {
        let sessions = [
            SessionLine {
                name: "b".to_owned(),
                host: None,
                project: Some("zeta".to_owned()),
            },
            SessionLine {
                name: "a".to_owned(),
                host: Some("edge".to_owned()),
                project: Some("alpha".to_owned()),
            },
            SessionLine {
                name: "c".to_owned(),
                host: None,
                project: None,
            },
            SessionLine {
                name: "d".to_owned(),
                host: None,
                project: Some("alpha".to_owned()),
            },
        ];
        let groups = group_by_project(&sessions);
        assert_eq!(
            groups
                .iter()
                .map(|group| group.project.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("alpha"), Some("zeta"), None]
        );
        assert_eq!(
            groups[0]
                .sessions
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "d"]
        );
    }

    #[test]
    fn adjacent_and_last_session_walk_the_real_lists() {
        let names = ["alpha", "beta", "gamma"];
        assert_eq!(adjacent_name(&names, "beta", 1), Some("gamma"));
        assert_eq!(adjacent_name(&names, "gamma", 1), Some("alpha"));
        assert_eq!(adjacent_name(&names, "alpha", -1), Some("gamma"));
        assert_eq!(adjacent_name(&["only"], "only", 1), None);
        let history = ["alpha", "beta", "alpha"];
        assert_eq!(last_name(&history, "alpha"), Some("beta"));
    }

    #[test]
    fn organization_lines_are_stable() {
        let lines = organization_lines(&[
            OrganizedSession {
                name: "zeta".to_owned(),
                host: None,
                directory: Some("/z".to_owned()),
                project: None,
            },
            OrganizedSession {
                name: "alpha".to_owned(),
                host: Some("edge".to_owned()),
                directory: Some("/a".to_owned()),
                project: Some("api".to_owned()),
            },
        ]);
        assert_eq!(
            lines,
            vec![
                "alpha host=edge directory=/a project=api".to_owned(),
                "zeta host=local directory=/z project=-".to_owned(),
            ]
        );
    }

    #[test]
    fn session_create_document_keeps_an_explicit_directory() {
        assert_eq!(
            session_create_document("work", Some("/src/api")),
            "{\"cwd\":\"/src/api\",\"keep_empty\":true,\"name\":\"work\"}"
        );
        assert_eq!(
            session_create_document("work", None),
            "{\"empty\":true,\"keep_empty\":true,\"name\":\"work\"}"
        );
    }
}
