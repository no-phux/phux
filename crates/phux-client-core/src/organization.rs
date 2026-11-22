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

#[cfg(test)]
mod tests {
    use super::{
        OrganizedSession, Place, PlaceSource, adjacent_name, last_name, organization_lines,
        place_for,
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
}
