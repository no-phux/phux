//! Project identity: the repository a session was created in (ADR-0168).
//!
//! A descriptive session attribute, never a container: sessions whose
//! [`SessionProject::id`] is equal belong to one repository, on any machine.
//! The identity is the `origin` remote normalized by
//! [`normalize_remote_url`], so two clones of one repository share it; a
//! checkout with no network `origin` falls back to [`path_project_id`], which
//! names its host. Pure data: discovery reads git files in `phux-client`.

use serde::{Deserialize, Serialize};

/// One session's project identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProject {
    /// Grouping key: a normalized remote (`github.com/a/b`) or
    /// `path:<host>:<canonical common git dir>`. Compare it; do not parse it.
    pub id: String,
    /// Display name: a `[[projects]]` label, else the repository name.
    pub name: String,
    /// The checkout (work tree) root on [`Self::host`].
    pub checkout: String,
    /// The branch checked out when the session was created. Descriptive
    /// only; it never groups or keys anything.
    #[serde(default)]
    pub branch: Option<String>,
    /// Whether the checkout is a linked worktree (its git dir differs from
    /// the common dir) rather than the primary checkout.
    #[serde(default)]
    pub linked_worktree: bool,
    /// The machine whose files the identity was read from.
    #[serde(default)]
    pub host: Option<String>,
}

/// Session attribute names (`docs/organization.md`) that carry an identity
/// on the wire: in `SESSION_CREATE`, organization records, and the
/// snapshot's session state.
pub mod attr {
    /// [`super::SessionProject::id`].
    pub const ID: &str = "project.id";
    /// [`super::SessionProject::name`].
    pub const NAME: &str = "project.name";
    /// [`super::SessionProject::checkout`].
    pub const CHECKOUT: &str = "project.checkout";
    /// [`super::SessionProject::branch`].
    pub const BRANCH: &str = "project.branch";
    /// [`super::SessionProject::linked_worktree`]: `true` or `false`.
    pub const LINKED_WORKTREE: &str = "project.linked_worktree";
    /// [`super::SessionProject::host`].
    pub const HOST: &str = "project.host";
    /// A session's explicit project link (`phux project link`): a display
    /// name that outranks the identity for grouping. Not part of the
    /// identity; [`super::SessionProject::from_attrs`] ignores it.
    pub const LINK: &str = "project.link";
}

/// Longest attribute value the wire carries, in UTF-8 bytes.
pub const MAX_ATTR_BYTES: usize = 256;

impl SessionProject {
    /// The identity as session attribute pairs. A required field over
    /// [`MAX_ATTR_BYTES`] yields no pairs; an optional one is left out.
    #[must_use]
    pub fn to_attrs(&self) -> Vec<(String, String)> {
        let fits = |value: &str| value.len() <= MAX_ATTR_BYTES;
        if !(fits(&self.id) && fits(&self.name) && fits(&self.checkout)) {
            return Vec::new();
        }
        let mut pairs = vec![
            (attr::ID.to_owned(), self.id.clone()),
            (attr::NAME.to_owned(), self.name.clone()),
            (attr::CHECKOUT.to_owned(), self.checkout.clone()),
            (
                attr::LINKED_WORKTREE.to_owned(),
                self.linked_worktree.to_string(),
            ),
        ];
        for (name, value) in [(attr::BRANCH, &self.branch), (attr::HOST, &self.host)] {
            if let Some(value) = value.as_ref().filter(|value| fits(value)) {
                pairs.push((name.to_owned(), value.clone()));
            }
        }
        pairs
    }

    /// The identity named by attribute pairs, or `None` without an id, name,
    /// and checkout. Other names are ignored.
    #[must_use]
    pub fn from_attrs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<Self> {
        let (mut id, mut name, mut checkout) = (None, None, None);
        let mut project = Self {
            id: String::new(),
            name: String::new(),
            checkout: String::new(),
            branch: None,
            linked_worktree: false,
            host: None,
        };
        for (key, value) in pairs {
            match key {
                attr::ID => id = Some(value.to_owned()),
                attr::NAME => name = Some(value.to_owned()),
                attr::CHECKOUT => checkout = Some(value.to_owned()),
                attr::BRANCH => project.branch = Some(value.to_owned()),
                attr::LINKED_WORKTREE => project.linked_worktree = value == "true",
                attr::HOST => project.host = Some(value.to_owned()),
                _ => {}
            }
        }
        project.id = id.filter(|id| !id.is_empty())?;
        project.name = name?;
        project.checkout = checkout?;
        Some(project)
    }
}

/// The identity of a checkout with no network remote: the canonical common
/// git dir, scoped to the host it was read on.
#[must_use]
pub fn path_project_id(host: &str, common_dir: &str) -> String {
    format!("path:{host}:{common_dir}")
}

/// Normalize a git remote URL so every spelling of one repository compares
/// equal: `git@github.com:a/b.git`, `ssh://git@github.com:22/a/b`, and
/// `https://user@GitHub.com/a/b/` all become `github.com/a/b`.
///
/// Scheme and userinfo are dropped, the host is lowercased, a default port
/// (22, 80, 443, 9418) is dropped and any other kept, and the path loses
/// empty segments and a trailing `.git`. Path case is kept. A local path or
/// `file://` remote is not a network identity and yields `None`, as does
/// anything without both a host and a path.
#[must_use]
pub fn normalize_remote_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((scheme, rest)) => split_scheme_url(&scheme.to_ascii_lowercase(), rest)?,
        None => split_scp_url(url)?,
    };
    let host = host.to_ascii_lowercase();
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(last) = segments.last_mut() {
        if let Some(stem) = last.strip_suffix(".git") {
            *last = stem;
        }
        if last.is_empty() {
            segments.pop();
        }
    }
    if host.is_empty() || segments.is_empty() {
        return None;
    }
    Some(format!("{host}/{}", segments.join("/")))
}

/// `scheme://[user@]host[:port]/path`, network schemes only.
fn split_scheme_url<'a>(scheme: &str, rest: &'a str) -> Option<(String, &'a str)> {
    let default_port = match scheme {
        "ssh" | "git+ssh" | "ssh+git" => "22",
        "http" => "80",
        "https" => "443",
        "git" => "9418",
        _ => return None,
    };
    let (authority, path) = rest.split_once('/')?;
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host.rsplit_once(':') {
        Some((name, port)) if port == default_port => name,
        _ => host,
    };
    Some((host.to_owned(), path))
}

/// scp-like `[user@]host:path`. A `/` before the first `:` means a local
/// path, and a one-letter host is a Windows drive.
fn split_scp_url(url: &str) -> Option<(String, &str)> {
    let colon = url.find(':')?;
    if url.find('/').is_some_and(|slash| slash < colon) {
        return None;
    }
    let authority = &url[..colon];
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host.len() < 2 {
        return None;
    }
    Some((host.to_owned(), &url[colon + 1..]))
}

/// The repository name a normalized remote or common dir ends in:
/// `github.com/a/phux` is `phux`; `/src/phux/.git` is `phux`; a bare
/// `/srv/phux.git` is `phux`.
#[must_use]
pub fn repo_name(path_like: &str) -> String {
    let mut segments = path_like
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty());
    let mut last = segments.next_back().unwrap_or_default();
    if last == ".git" {
        last = segments.next_back().unwrap_or(last);
    }
    last.strip_suffix(".git")
        .filter(|stem| !stem.is_empty())
        .unwrap_or(last)
        .to_owned()
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_one_github_repo_is_one_identity() {
        let want = Some("github.com/a/b".to_owned());
        for url in [
            "git@github.com:a/b.git",
            "git@github.com:a/b",
            "https://github.com/a/b",
            "https://github.com/a/b.git",
            "https://github.com/a/b/",
            "https://user:token@GitHub.com/a/b.git",
            "http://github.com:80/a/b",
            "ssh://git@github.com/a/b.git",
            "ssh://git@github.com:22/a/b.git",
            "git+ssh://git@github.com/a/b",
            "git://github.com/a/b.git",
            "  git@GITHUB.com:a/b.git\n",
        ] {
            assert_eq!(normalize_remote_url(url), want, "{url}");
        }
    }

    #[test]
    fn path_case_and_non_default_ports_are_kept() {
        assert_eq!(
            normalize_remote_url("ssh://git@git.example.com:2222/Team/Repo.git").as_deref(),
            Some("git.example.com:2222/Team/Repo")
        );
        assert_eq!(
            normalize_remote_url("https://gitlab.com/group/sub/project.git").as_deref(),
            Some("gitlab.com/group/sub/project")
        );
        assert_eq!(
            normalize_remote_url("https://[::1]:443/a/b").as_deref(),
            Some("[::1]/a/b")
        );
    }

    #[test]
    fn local_remotes_are_not_network_identities() {
        for url in [
            "/srv/git/repo.git",
            "./repo",
            "../repo.git",
            "file:///srv/git/repo.git",
            "C:\\src\\repo",
            "C:/src/repo",
            "",
            "github.com",
            "https://github.com/",
            "git@github.com:",
            "git@github.com:.git",
        ] {
            assert_eq!(normalize_remote_url(url), None, "{url:?}");
        }
    }

    #[test]
    fn repo_names_come_from_the_last_segment() {
        assert_eq!(repo_name("github.com/a/phux"), "phux");
        assert_eq!(repo_name("/src/phux/.git"), "phux");
        assert_eq!(repo_name("/srv/phux.git"), "phux");
        assert_eq!(repo_name("/srv/phux.git/"), "phux");
        assert_eq!(repo_name(".git"), ".git");
    }

    #[test]
    fn path_ids_name_their_host() {
        assert_eq!(
            path_project_id("mini", "/src/phux/.git"),
            "path:mini:/src/phux/.git"
        );
    }

    fn sample() -> SessionProject {
        SessionProject {
            id: "github.com/no-phux/phux".to_owned(),
            name: "phux".to_owned(),
            checkout: "/w/phux-feat".to_owned(),
            branch: Some("feat".to_owned()),
            linked_worktree: true,
            host: Some("mini".to_owned()),
        }
    }

    fn pairs(attrs: &[(String, String)]) -> impl Iterator<Item = (&str, &str)> {
        attrs.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    #[test]
    fn an_identity_round_trips_through_attribute_pairs() {
        let project = sample();
        let attrs = project.to_attrs();
        assert_eq!(SessionProject::from_attrs(pairs(&attrs)), Some(project));
        let bare = SessionProject {
            branch: None,
            host: None,
            linked_worktree: false,
            ..sample()
        };
        let attrs = bare.to_attrs();
        assert_eq!(attrs.len(), 4);
        assert_eq!(SessionProject::from_attrs(pairs(&attrs)), Some(bare));
    }

    #[test]
    fn missing_or_oversized_fields_carry_no_identity() {
        let long = "x".repeat(MAX_ATTR_BYTES + 1);
        let too_long = SessionProject {
            checkout: long.clone(),
            ..sample()
        };
        assert!(too_long.to_attrs().is_empty());
        let long_branch = SessionProject {
            branch: Some(long),
            ..sample()
        };
        let attrs = long_branch.to_attrs();
        assert_eq!(
            SessionProject::from_attrs(pairs(&attrs)).and_then(|p| p.branch),
            None
        );
        assert_eq!(
            SessionProject::from_attrs([("project", "phux"), (attr::ID, "x")]),
            None
        );
    }

    #[test]
    fn a_project_link_is_not_part_of_the_identity() {
        let mut attrs = sample().to_attrs();
        attrs.push((attr::LINK.to_owned(), "elsewhere".to_owned()));
        assert_eq!(SessionProject::from_attrs(pairs(&attrs)), Some(sample()));
        assert_eq!(
            SessionProject::from_attrs([(attr::LINK, "phux"), (attr::NAME, "phux")]),
            None
        );
    }
}
