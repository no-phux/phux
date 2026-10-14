//! Project identity discovery (ADR-0167).
//!
//! Discovery reads git's own files: the `.git` directory or `gitdir:` file,
//! `commondir`, `HEAD`, and the common `config`. It runs no `git` process,
//! so it is cheap and the server never needs git. The identity rides as
//! typed session attributes: set by `SESSION_CREATE`, read back from each
//! session's snapshot state ([`of_session`]).

use std::path::{Path, PathBuf};

use phux_core::project_identity::{
    SessionProject, normalize_remote_url, path_project_id, repo_name,
};
use phux_protocol::wire::info::SessionInfo;

/// Where a checkout's git state lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCheckout {
    /// The work tree root; the repository itself for a bare repository.
    pub root: PathBuf,
    /// This checkout's git dir (`.git`, or `.git/worktrees/<name>`).
    pub git_dir: PathBuf,
    /// The git dir every worktree of the repository shares.
    pub common_dir: PathBuf,
}

impl GitCheckout {
    /// A linked worktree, as opposed to the primary checkout.
    #[must_use]
    pub fn is_linked_worktree(&self) -> bool {
        self.git_dir != self.common_dir
    }
}

/// The checkout `start` sits in: the nearest ancestor with a `.git`
/// directory or `gitdir:` file, or a git dir itself (a bare repository, or a
/// path inside `.git`). `None` outside git or when a path cannot be read.
#[must_use]
pub fn find_checkout(start: &Path) -> Option<GitCheckout> {
    let start = start.canonicalize().ok()?;
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        let (root, git_dir) = if dot_git.is_dir() {
            (dir.to_path_buf(), dot_git)
        } else if dot_git.is_file() {
            (dir.to_path_buf(), read_gitdir_file(&dot_git, dir)?)
        } else if is_git_dir(dir) {
            let root = if dir.file_name().is_some_and(|name| name == ".git") {
                dir.parent()?.to_path_buf()
            } else {
                dir.to_path_buf()
            };
            (root, dir.to_path_buf())
        } else {
            continue;
        };
        let git_dir = git_dir.canonicalize().ok()?;
        let common_dir = read_common_dir(&git_dir)?;
        return Some(GitCheckout {
            root,
            git_dir,
            common_dir,
        });
    }
    None
}

/// `gitdir: <path>` in a worktree's `.git` file, relative to its directory.
fn read_gitdir_file(file: &Path, dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(file).ok()?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    (!target.is_empty()).then(|| dir.join(target))
}

/// A git dir has a `HEAD` file and either its own `objects` or a
/// `commondir` pointing at them.
fn is_git_dir(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && (dir.join("objects").is_dir() || dir.join("commondir").is_file())
}

/// The common dir a linked worktree's `commondir` names (relative to its git
/// dir), else the git dir itself.
fn read_common_dir(git_dir: &Path) -> Option<PathBuf> {
    std::fs::read_to_string(git_dir.join("commondir")).map_or_else(
        |_| Some(git_dir.to_path_buf()),
        |text| git_dir.join(text.trim()).canonicalize().ok(),
    )
}

/// The branch `HEAD` names, or `None` when detached or unreadable.
#[must_use]
pub fn head_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let target = head.trim().strip_prefix("ref:")?.trim();
    Some(
        target
            .strip_prefix("refs/heads/")
            .unwrap_or(target)
            .to_owned(),
    )
}

/// The first `url` of `[remote "origin"]` in a git config file's text.
/// Includes and `insteadOf` rewrites are not applied.
#[must_use]
pub fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for raw in config.lines() {
        let mut line = raw.trim();
        if line.starts_with('[') {
            let Some(close) = line.find(']') else {
                in_origin = false;
                continue;
            };
            in_origin = is_origin_header(&line[1..close]);
            line = line[close + 1..].trim();
        }
        if !in_origin || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            return Some(config_value(value.trim()));
        }
    }
    None
}

/// `remote "origin"`, or the legacy `remote.origin` spelling.
fn is_origin_header(header: &str) -> bool {
    let header = header.trim();
    if let Some((section, sub)) = header.split_once(char::is_whitespace) {
        return section.eq_ignore_ascii_case("remote") && sub.trim().trim_matches('"') == "origin";
    }
    header.eq_ignore_ascii_case("remote.origin")
}

/// A config value without surrounding quotes or a trailing comment.
fn config_value(value: &str) -> String {
    if let Some(quoted) = value.strip_prefix('"') {
        return quoted.split('"').next().unwrap_or_default().to_owned();
    }
    let end = value
        .find([' ', '\t'])
        .filter(|at| value[*at..].trim_start().starts_with(['#', ';']))
        .unwrap_or(value.len());
    value[..end].trim().to_owned()
}

/// The project identity of the checkout `dir` sits in, read on `host`.
/// `label` (a `[[projects]]` name) wins over the repository name.
#[must_use]
pub fn discover(dir: &Path, host: &str, label: Option<&str>) -> Option<SessionProject> {
    let checkout = find_checkout(dir)?;
    let common = checkout.common_dir.to_string_lossy().into_owned();
    let remote = std::fs::read_to_string(checkout.common_dir.join("config"))
        .ok()
        .as_deref()
        .and_then(origin_url)
        .as_deref()
        .and_then(normalize_remote_url);
    let name = label.filter(|label| !label.trim().is_empty()).map_or_else(
        || repo_name(remote.as_deref().unwrap_or(&common)),
        ToOwned::to_owned,
    );
    Some(SessionProject {
        id: remote.unwrap_or_else(|| path_project_id(host, &common)),
        name,
        checkout: checkout.root.to_string_lossy().into_owned(),
        branch: head_branch(&checkout.git_dir),
        linked_worktree: checkout.is_linked_worktree(),
        host: Some(host.to_owned()),
    })
}

/// The `SESSION_CREATE` attributes for a session created in `dir`, read on
/// `host`: the identity's pairs, or none outside git.
#[must_use]
pub fn creation_attrs(dir: &Path, host: &str, label: Option<&str>) -> Vec<(String, String)> {
    discover(dir, host, label).map_or_else(Vec::new, |project| project.to_attrs())
}

/// The identity a listed session's attributes carry.
#[must_use]
pub fn of_session(session: &SessionInfo) -> Option<SessionProject> {
    SessionProject::from_attrs(
        session
            .attrs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("run git");
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    /// A repository with one commit on `main`, `origin` set to `remote`.
    fn repo(parent: &Path, remote: Option<&str>) -> PathBuf {
        let root = parent.join("phux");
        std::fs::create_dir_all(&root).expect("mkdir");
        git(&root, &["init", "-q", "-b", "main"]);
        git(
            &root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        if let Some(url) = remote {
            git(&root, &["remote", "add", "origin", url]);
        }
        root.canonicalize().expect("canonical root")
    }

    #[test]
    fn the_primary_checkout_and_a_linked_worktree_share_the_remote_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = repo(dir.path(), Some("git@github.com:no-phux/phux.git"));
        let linked = dir.path().join("phux-feat");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                linked.to_str().unwrap(),
            ],
        );
        std::fs::create_dir_all(linked.join("src/deep")).expect("mkdir");

        let primary = discover(&root, "mini", None).expect("primary");
        assert_eq!(primary.id, "github.com/no-phux/phux");
        assert_eq!(primary.name, "phux");
        assert!(!primary.linked_worktree);
        assert_eq!(primary.branch.as_deref(), Some("main"));
        assert_eq!(primary.checkout, root.to_string_lossy());

        let child = discover(&linked.join("src/deep"), "mini", None).expect("linked");
        assert_eq!(child.id, primary.id);
        assert!(child.linked_worktree);
        assert_eq!(child.branch.as_deref(), Some("feat"));
        assert_eq!(
            child.checkout,
            linked.canonicalize().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn without_a_remote_the_identity_is_the_host_scoped_common_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = repo(dir.path(), None);
        let linked = dir.path().join("phux-wt");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt",
                linked.to_str().unwrap(),
            ],
        );
        let common = root.join(".git").to_string_lossy().into_owned();
        let primary = discover(&root, "mini", None).expect("primary");
        assert_eq!(primary.id, format!("path:mini:{common}"));
        assert_eq!(primary.name, "phux");
        let child = discover(&linked, "mini", None).expect("linked");
        assert_eq!(child.id, primary.id);
        assert!(child.linked_worktree);
        assert_ne!(
            discover(&root, "laptop", None).expect("other host").id,
            primary.id
        );
    }

    #[test]
    fn a_bare_repository_is_its_own_common_dir_and_its_worktrees_are_linked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = repo(dir.path(), None);
        let bare = dir.path().join("phux.git");
        git(
            dir.path(),
            &[
                "clone",
                "-q",
                "--bare",
                source.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        git(&bare, &["remote", "remove", "origin"]);
        git(
            &bare,
            &["remote", "add", "origin", "https://github.com/no-phux/phux"],
        );
        let inside = discover(&bare, "mini", None).expect("bare");
        assert_eq!(inside.id, "github.com/no-phux/phux");
        assert!(!inside.linked_worktree);

        let wt = dir.path().join("wt");
        git(
            &bare,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "main"],
        );
        let child = discover(&wt, "mini", None).expect("worktree of bare");
        assert_eq!(child.id, inside.id);
        assert!(child.linked_worktree);
        assert_eq!(child.branch.as_deref(), Some("main"));
        let checkout = find_checkout(&wt).expect("checkout");
        assert_eq!(checkout.common_dir, bare.canonicalize().unwrap());
    }

    #[test]
    fn a_gitdir_file_is_followed_by_hand() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("store");
        std::fs::create_dir_all(real.join("objects")).expect("mkdir");
        std::fs::create_dir_all(real.join("refs")).expect("mkdir");
        std::fs::write(real.join("HEAD"), "ref: refs/heads/trunk\n").expect("HEAD");
        std::fs::write(
            real.join("config"),
            "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = \"ssh://git@GitHub.com:22/a/b.git\" ; note\n",
        )
        .expect("config");
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("sub")).expect("mkdir");
        std::fs::write(work.join(".git"), "gitdir: ../store\n").expect(".git file");

        let project = discover(&work.join("sub"), "mini", Some("Main repo")).expect("project");
        assert_eq!(project.id, "github.com/a/b");
        assert_eq!(project.name, "Main repo");
        assert_eq!(project.branch.as_deref(), Some("trunk"));
        assert!(!project.linked_worktree);
        assert_eq!(
            project.checkout,
            work.canonicalize().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn outside_git_no_checkout_starts_here() {
        let dir = tempfile::tempdir().expect("tempdir");
        let here = dir.path().canonicalize().expect("canonical");
        // A temp dir inside some enclosing repository still finds that one,
        // never a checkout rooted at the temp dir itself.
        assert!(find_checkout(&here).is_none_or(|found| found.root != here));
    }

    #[test]
    fn origin_url_reads_the_origin_section_only() {
        let config = "[remote \"upstream\"]\n\turl = git@x:u/v\n[Remote \"origin\"]\n\tfetch = +refs/*\n\tURL = https://h/a/b # c\n\turl = second\n";
        assert_eq!(origin_url(config).as_deref(), Some("https://h/a/b"));
        assert_eq!(
            origin_url("[remote.origin]\nurl=git@h:a/b\n").as_deref(),
            Some("git@h:a/b")
        );
        assert_eq!(origin_url("[remote \"up\"]\nurl = x\n"), None);
    }

    #[test]
    fn head_branch_reads_symbolic_refs_and_not_detached_heads() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("HEAD"), "ref: refs/heads/feat/x\n").expect("HEAD");
        assert_eq!(head_branch(dir.path()).as_deref(), Some("feat/x"));
        std::fs::write(dir.path().join("HEAD"), "0123abcd\n").expect("HEAD");
        assert_eq!(head_branch(dir.path()), None);
    }

    #[test]
    fn a_created_session_reads_its_identity_back_from_its_attributes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = repo(dir.path(), Some("git@github.com:no-phux/phux.git"));
        let attrs = creation_attrs(&root, "mini", Some("Phux"));
        let session =
            SessionInfo::new(phux_protocol::ids::SessionId::new(1), "phux").with_attrs(attrs);
        let project = of_session(&session).expect("identity");
        assert_eq!(
            project,
            discover(&root, "mini", Some("Phux")).expect("discover")
        );
        assert_eq!(project.name, "Phux");
        let outside = tempfile::tempdir().expect("tempdir");
        if find_checkout(outside.path()).is_none() {
            assert!(creation_attrs(outside.path(), "mini", None).is_empty());
        }
        assert_eq!(
            of_session(&SessionInfo::new(
                phux_protocol::ids::SessionId::new(2),
                "x"
            )),
            None
        );
    }
}
