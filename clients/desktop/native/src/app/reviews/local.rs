//! Which checkout on this Mac holds a pull request's repository, which
//! worktree is on its branch, and which agent pane works there.
//!
//! Candidates are the folders local panes report, the folders opened from the
//! app, and the usual places people keep clones. Each is read from its `.git`
//! files alone, without running `git`.

use std::path::{Path, PathBuf};

use phux_client_runtime::project_identity::discover;
use phux_protocol::ids::ResourceId;

/// A local pane: its terminal, its title, and whether an agent runs there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pane {
    pub terminal: ResourceId,
    pub title: String,
    pub agent: bool,
}

/// A folder worth reading, and the pane that reported it.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub dir: PathBuf,
    pub pane: Option<Pane>,
}

/// One checkout of a repository on this Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub root: PathBuf,
    pub branch: Option<String>,
    pub linked: bool,
    /// The pane in it, an agent's first.
    pub pane: Option<Pane>,
}

/// Where clones usually live, for `owner/name`.
pub(crate) fn conventional(home: &Path, repo: &str) -> Vec<PathBuf> {
    let Some((owner, name)) = crate::git::github::split_repo(repo) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for base in [
        "workspace",
        "src",
        "code",
        "dev",
        "projects",
        "repos",
        "git",
        "github",
        "Developer",
    ] {
        let base = home.join(base);
        dirs.push(base.join(owner).join(name));
        dirs.push(base.join(name));
        dirs.push(base.join("github.com").join(owner).join(name));
    }
    dirs.push(home.join(name));
    dirs
}

/// Every candidate checkout of `repo` (`owner/name`, any case).
pub(crate) fn find(repo: &str, candidates: &[Candidate], host: &str) -> Vec<Found> {
    let want = format!("github.com/{}", repo.to_ascii_lowercase());
    let mut found: Vec<Found> = Vec::new();
    for candidate in candidates {
        if !candidate.dir.is_dir() {
            continue;
        }
        let Some(project) = discover(&candidate.dir, host, None) else {
            continue;
        };
        if project.id.to_ascii_lowercase() != want {
            continue;
        }
        let root = PathBuf::from(&project.checkout);
        match found.iter_mut().find(|each| each.root == root) {
            Some(each) => {
                if outranks(candidate.pane.as_ref(), each.pane.as_ref()) {
                    each.pane = candidate.pane.clone();
                }
            }
            None => found.push(Found {
                root,
                branch: project.branch,
                linked: project.linked_worktree,
                pane: candidate.pane.clone(),
            }),
        }
    }
    found
}

/// A pane beats none, and an agent pane beats a plain shell.
fn outranks(new: Option<&Pane>, old: Option<&Pane>) -> bool {
    match (new, old) {
        (Some(_), None) => true,
        (Some(new), Some(old)) => new.agent && !old.agent,
        _ => false,
    }
}

/// The clone to read from: a primary checkout before a linked worktree, and
/// then the first found.
pub(crate) fn clone_of(found: &[Found]) -> Option<&Found> {
    found
        .iter()
        .find(|each| !each.linked)
        .or_else(|| found.first())
}

/// The checkout already on `branch`, an agent's first.
pub(crate) fn on_branch<'a>(found: &'a [Found], branch: &str) -> Option<&'a Found> {
    let on = |each: &&Found| each.branch.as_deref() == Some(branch);
    found
        .iter()
        .filter(on)
        .find(|each| each.pane.as_ref().is_some_and(|pane| pane.agent))
        .or_else(|| found.iter().find(on))
}
