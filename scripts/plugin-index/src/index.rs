//! Assemble the index from GitHub (or fixtures) and the local examples.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::github::{self, CatalogMeta, Fetched, RepoMeta};
use crate::validate::{self, Facts};
use crate::{Error, Index, NOTICE, Options, Plugin, SCHEMA_VERSION, Skipped};

pub(crate) fn build(options: &Options) -> Result<Index, Error> {
    if !valid_stamp(&options.generated_at) {
        return Err(Error::new("generated_at must be YYYY-MM-DDTHH:MM:SSZ"));
    }
    if !github::valid_repo_name(&options.examples_repo) {
        return Err(Error::new(format!(
            "examples repository {} is not owner/repo",
            options.examples_repo
        )));
    }
    let repos = github::search(&options.source)?;
    let mut plugins = Vec::new();
    let mut skipped = Vec::new();
    let read = |repo: &RepoMeta, path: &str| github::read_text(&options.source, repo, path);
    for repo in &repos {
        index_repo(&read, repo, &mut plugins, &mut skipped)?;
    }
    add_examples(options, &repos, &mut plugins, &mut skipped)?;
    plugins.sort_by(|left, right| {
        (&left.repo, &left.subdir, &left.name, &left.id).cmp(&(
            &right.repo,
            &right.subdir,
            &right.name,
            &right.id,
        ))
    });
    skipped.sort_by(|left, right| {
        (&left.repo, &left.subdir, &left.reason).cmp(&(&right.repo, &right.subdir, &right.reason))
    });
    Ok(Index {
        schema_version: SCHEMA_VERSION,
        generated_at: options.generated_at.clone(),
        notice: NOTICE.to_owned(),
        plugins,
        skipped,
    })
}

fn index_repo<R>(
    read: &R,
    repo: &RepoMeta,
    plugins: &mut Vec<Plugin>,
    skipped: &mut Vec<Skipped>,
) -> Result<(), Error>
where
    R: Fn(&RepoMeta, &str) -> Result<Fetched, Error>,
{
    if repo.default_branch.is_empty() {
        skipped.push(skip(repo, "", "missing default branch"));
        return Ok(());
    }
    if repo.archived {
        skipped.push(skip(repo, "", "archived"));
        return Ok(());
    }
    if repo.fork {
        skipped.push(skip(repo, "", "fork"));
        return Ok(());
    }
    if repo.private {
        skipped.push(skip(repo, "", "private"));
        return Ok(());
    }
    let meta = CatalogMeta::from(repo);
    let root = take(read, repo, "phux-plugin.toml", "", skipped)?;
    let dirs = take(read, repo, "phux-plugin-dirs.toml", "", skipped)?;
    let mut listed = false;
    if let Got::Text(text) = &root {
        listed = true;
        match materialize_and_judge(read, repo, "", text) {
            Ok(Ok(facts)) => plugins.push(plugin_from(facts, &meta, &repo.full_name, "")),
            Ok(Err(reason)) => skipped.push(skip(repo, "", &reason)),
            Err(err) => return Err(err),
        }
    }
    match dirs {
        Got::Missing => {
            if matches!(root, Got::Missing) {
                skipped.push(skip(repo, "", "no valid manifest"));
            }
        }
        Got::Failed => {}
        Got::Text(text) => match parse_dirs(&text) {
            Err(reason) => skipped.push(skip(repo, "", &reason)),
            Ok(subdirs) => {
                if subdirs.is_empty() && !listed {
                    skipped.push(skip(repo, "", "no valid manifest"));
                }
                let mut seen = std::collections::BTreeSet::new();
                for raw in subdirs {
                    if !seen.insert(raw.clone()) {
                        continue;
                    }
                    match validate::safe_relative(&raw) {
                        Err(reason) => skipped.push(skip(repo, "", &reason)),
                        Ok(subdir) => {
                            let path = format!("{subdir}/phux-plugin.toml");
                            match take(read, repo, &path, &subdir, skipped)? {
                                Got::Missing => {
                                    skipped.push(skip(repo, &subdir, "no valid manifest"))
                                }
                                Got::Failed => {}
                                Got::Text(body) => {
                                    match materialize_and_judge(read, repo, &subdir, &body) {
                                        Ok(Ok(facts)) => plugins.push(plugin_from(
                                            facts,
                                            &meta,
                                            &repo.full_name,
                                            &subdir,
                                        )),
                                        Ok(Err(reason)) => {
                                            skipped.push(skip(repo, &subdir, &reason))
                                        }
                                        Err(err) => return Err(err),
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    }
    Ok(())
}

fn materialize_and_judge<R>(
    read: &R,
    repo: &RepoMeta,
    subdir: &str,
    manifest: &str,
) -> Result<Result<Facts, String>, Error>
where
    R: Fn(&RepoMeta, &str) -> Result<Fetched, Error>,
{
    let logical = logical_root(&repo.full_name, subdir);
    let temp = match validate::scratch() {
        Ok(temp) => temp,
        Err(err) => return Ok(Err(err.to_string())),
    };
    let manifest_path = temp.path().join("phux-plugin.toml");
    if let Err(err) = fs::write(&manifest_path, manifest) {
        return Ok(Err(format!("could not store manifest: {err}")));
    }
    for relative in validate::theme_color_rels(manifest) {
        let remote = if subdir.is_empty() {
            relative.clone()
        } else {
            format!("{subdir}/{relative}")
        };
        let body = match read(repo, &remote) {
            Ok(Fetched::Text(body)) => body,
            Ok(Fetched::Missing) => continue,
            Ok(Fetched::Skip(reason)) => return Ok(Err(reason)),
            Err(err) => return Err(err),
        };
        let destination = temp.path().join(&relative);
        if let Some(parent) = destination.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                return Ok(Err(format!("could not store manifest: {err}")));
            }
        }
        if let Err(err) = fs::write(&destination, body) {
            return Ok(Err(format!("could not store manifest: {err}")));
        }
    }
    Ok(validate::judge(&manifest_path, &logical))
}

enum Got {
    Text(String),
    Missing,
    Failed,
}

fn take<R>(
    read: &R,
    repo: &RepoMeta,
    path: &str,
    subdir: &str,
    skipped: &mut Vec<Skipped>,
) -> Result<Got, Error>
where
    R: Fn(&RepoMeta, &str) -> Result<Fetched, Error>,
{
    match read(repo, path) {
        Ok(Fetched::Text(text)) => Ok(Got::Text(text)),
        Ok(Fetched::Missing) => Ok(Got::Missing),
        Ok(Fetched::Skip(reason)) => {
            skipped.push(skip(repo, subdir, &reason));
            Ok(Got::Failed)
        }
        Err(err) => Err(err),
    }
}

fn add_examples(
    options: &Options,
    repos: &[RepoMeta],
    plugins: &mut Vec<Plugin>,
    skipped: &mut Vec<Skipped>,
) -> Result<(), Error> {
    if !options.examples_dir.is_dir() {
        return Err(Error::new(format!(
            "examples directory {} does not exist",
            options.examples_dir.display()
        )));
    }
    let meta = if let Some(repo) = repos
        .iter()
        .find(|repo| repo.full_name == options.examples_repo)
    {
        CatalogMeta::from(repo)
    } else {
        match github::fetch_repo(&options.source, &options.examples_repo)? {
            Some(repo) => CatalogMeta::from(&repo),
            None => CatalogMeta {
                stars: None,
                pushed_at: None,
                license: None,
            },
        }
    };
    let mut children = fs::read_dir(&options.examples_dir)
        .map_err(|err| Error::new(format!("reading {}: {err}", options.examples_dir.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| Error::new(err.to_string()))?;
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let subdir = example_subdir(&options.repo_root, &path)?;
        plugins.retain(|plugin| plugin.repo != options.examples_repo || plugin.subdir != subdir);
        skipped.retain(|skip| skip.repo != options.examples_repo || skip.subdir != subdir);
        let manifest = path.join("phux-plugin.toml");
        if !manifest.is_file() {
            skipped.push(Skipped {
                repo: options.examples_repo.clone(),
                subdir,
                reason: "no valid manifest".to_owned(),
            });
            continue;
        }
        let logical = logical_root(&options.examples_repo, &subdir);
        match validate::judge(&manifest, &logical) {
            Ok(facts) => plugins.push(plugin_from(facts, &meta, &options.examples_repo, &subdir)),
            Err(reason) => skipped.push(Skipped {
                repo: options.examples_repo.clone(),
                subdir,
                reason,
            }),
        }
    }
    Ok(())
}

fn example_subdir(repo_root: &Path, plugin_dir: &Path) -> Result<String, Error> {
    let root = repo_root
        .canonicalize()
        .map_err(|err| Error::new(format!("examples root {}: {err}", repo_root.display())))?;
    let plugin = plugin_dir
        .canonicalize()
        .map_err(|err| Error::new(format!("example {}: {err}", plugin_dir.display())))?;
    let relative = plugin.strip_prefix(&root).map_err(|_| {
        Error::new(format!(
            "example {} is outside {}",
            plugin.display(),
            root.display()
        ))
    })?;
    let text = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    validate::safe_relative(&text).map_err(Error::new)
}

fn plugin_from(facts: Facts, meta: &CatalogMeta, repo: &str, subdir: &str) -> Plugin {
    Plugin {
        repo: repo.to_owned(),
        subdir: subdir.to_owned(),
        id: facts.id,
        name: facts.name,
        description: facts.description,
        kinds: facts.kinds,
        min_phux_version: facts.min_phux_version,
        stars: meta.stars,
        pushed_at: meta.pushed_at.clone(),
        license: meta.license.clone(),
        install: format!("phux plugin install https://github.com/{repo}"),
    }
}

const REASON_MAX_CHARS: usize = 300;

fn skip(repo: &RepoMeta, subdir: &str, reason: &str) -> Skipped {
    Skipped {
        repo: repo.full_name.clone(),
        subdir: subdir.to_owned(),
        reason: validate::cap_field(reason, REASON_MAX_CHARS),
    }
}

fn logical_root(repo: &str, subdir: &str) -> String {
    if subdir.is_empty() {
        repo.to_owned()
    } else {
        format!("{repo}/{subdir}")
    }
}

#[derive(Deserialize)]
struct DirsFile {
    subdirs: Vec<String>,
}

fn parse_dirs(text: &str) -> Result<Vec<String>, String> {
    match toml::from_str::<DirsFile>(text) {
        Ok(file) => Ok(file.subdirs),
        Err(err) => Err(format!(
            "invalid plugin directory list: {}",
            validate::one_line(&err.to_string())
        )),
    }
}

pub(crate) fn valid_stamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 20 || !text.ends_with('Z') {
        return false;
    }
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    separators
        .iter()
        .all(|(index, expected)| bytes[*index] == *expected)
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_to_the_second() {
        assert!(valid_stamp("2026-01-15T00:00:00Z"));
        assert!(!valid_stamp("2026-01-15T00:00:00+00:00"));
        assert!(!valid_stamp("2026-01-15"));
    }

    #[test]
    fn one_hostile_repo_does_not_abort_the_index() {
        let root = validate::scratch().expect("temp");
        let fixtures = root.path().join("fixtures");
        write_plugin(&fixtures, "acme/good", b"id = \"acme.good\"\nname = \"Good\"\nversion = \"0.1.0\"\nmin_phux_version = \"0.0.2\"\n");
        write_bytes(&fixtures, "acme/binary", &[0xff, 0xfe, 0xfd]);
        write_bytes(&fixtures, "acme/huge", &vec![b'a'; (1024 * 1024) + 1]);
        let dir_manifest = fixtures.join("repos/acme/dir/phux-plugin.toml");
        fs::create_dir_all(&dir_manifest).expect("directory manifest");
        let offline = crate::Source::Offline { fixtures };
        let read = |repo: &RepoMeta, path: &str| {
            if repo.full_name == "acme/denied" {
                return github::classify_body(403, "", b"forbidden".to_vec(), 1024 * 1024);
            }
            github::read_text(&offline, repo, path)
        };
        let mut plugins = Vec::new();
        let mut skipped = Vec::new();
        for name in [
            "acme/good",
            "acme/binary",
            "acme/huge",
            "acme/dir",
            "acme/denied",
        ] {
            index_repo(&read, &sample_repo(name), &mut plugins, &mut skipped).expect("index");
        }
        assert_eq!(plugins.len(), 1, "{skipped:?}");
        assert_eq!(plugins[0].name, "Good");
        let reasons: Vec<&str> = skipped.iter().map(|skip| skip.reason.as_str()).collect();
        assert!(
            reasons.iter().any(|reason| reason.contains("not UTF-8")),
            "{reasons:?}"
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("exceeds 1 MiB")),
            "{reasons:?}"
        );
        assert!(
            reasons.iter().any(|reason| reason.contains("directory")),
            "{reasons:?}"
        );
        assert!(reasons.contains(&"HTTP 403"), "{reasons:?}");
    }

    #[test]
    fn a_read_error_aborts_instead_of_being_skipped() {
        let read = |_repo: &RepoMeta, _path: &str| Err(Error::new("transport down"));
        let mut plugins = Vec::new();
        let mut skipped = Vec::new();
        let err = index_repo(&read, &sample_repo("acme/good"), &mut plugins, &mut skipped)
            .expect_err("transport");
        assert!(plugins.is_empty());
        assert!(
            skipped.is_empty(),
            "an Err read must not be recorded as a skip: {skipped:?}"
        );
        assert!(err.to_string().contains("transport down"), "{err}");
    }

    #[test]
    fn a_theme_fetch_error_aborts_instead_of_being_skipped() {
        let manifest = "\
id = \"acme.theme\"
name = \"Theme\"
version = \"0.1.0\"
min_phux_version = \"0.0.2\"

[[themes]]
name = \"demo\"
path = \"themes/demo\"
";
        let read = |_repo: &RepoMeta, path: &str| {
            if path.ends_with("colors.toml") {
                Err(Error::new("theme transport down"))
            } else if path == "phux-plugin.toml" {
                Ok(Fetched::Text(manifest.to_owned()))
            } else {
                Ok(Fetched::Missing)
            }
        };
        let mut plugins = Vec::new();
        let mut skipped = Vec::new();
        let err = index_repo(
            &read,
            &sample_repo("acme/theme"),
            &mut plugins,
            &mut skipped,
        )
        .expect_err("theme transport");
        assert!(plugins.is_empty());
        assert!(
            skipped.is_empty(),
            "a theme fetch Err must not be recorded as a skip: {skipped:?}"
        );
        assert!(err.to_string().contains("theme transport down"), "{err}");
    }

    #[test]
    fn skip_reasons_are_capped() {
        let reason = "x".repeat(REASON_MAX_CHARS + 40);
        let skipped = skip(&sample_repo("acme/long"), "", &reason);
        assert_eq!(skipped.reason.chars().count(), REASON_MAX_CHARS);
    }

    fn sample_repo(name: &str) -> RepoMeta {
        RepoMeta {
            full_name: name.to_owned(),
            archived: false,
            fork: false,
            private: false,
            default_branch: "main".to_owned(),
            stars: 1,
            pushed_at: "2026-01-01T00:00:00Z".to_owned(),
            license: None,
        }
    }

    fn write_plugin(fixtures: &Path, repo: &str, body: &[u8]) {
        write_bytes(fixtures, repo, body);
    }

    fn write_bytes(fixtures: &Path, repo: &str, body: &[u8]) {
        let path = fixtures.join("repos").join(repo).join("phux-plugin.toml");
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(path, body).expect("write");
    }
}
