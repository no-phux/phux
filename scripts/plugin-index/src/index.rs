//! Assemble the index from GitHub (or fixtures) and the local examples.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::github::{self, CatalogMeta, RepoMeta};
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
    for repo in &repos {
        index_repo(&options.source, repo, &mut plugins, &mut skipped)?;
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

fn index_repo(
    source: &crate::Source,
    repo: &RepoMeta,
    plugins: &mut Vec<Plugin>,
    skipped: &mut Vec<Skipped>,
) -> Result<(), Error> {
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
    let root = github::read_text(source, repo, "phux-plugin.toml")?;
    let dirs = github::read_text(source, repo, "phux-plugin-dirs.toml")?;
    let mut listed = false;
    if let Some(text) = &root {
        listed = true;
        match materialize_and_judge(source, repo, "", text)? {
            Ok(facts) => plugins.push(plugin_from(facts, &meta, &repo.full_name, "")),
            Err(reason) => skipped.push(skip(repo, "", &reason)),
        }
    }
    match dirs {
        None => {
            if root.is_none() {
                skipped.push(skip(repo, "", "no valid manifest"));
            }
        }
        Some(text) => match parse_dirs(&text) {
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
                            match github::read_text(source, repo, &path)? {
                                None => skipped.push(skip(repo, &subdir, "no valid manifest")),
                                Some(body) => {
                                    match materialize_and_judge(source, repo, &subdir, &body)? {
                                        Ok(facts) => plugins.push(plugin_from(
                                            facts,
                                            &meta,
                                            &repo.full_name,
                                            &subdir,
                                        )),
                                        Err(reason) => skipped.push(skip(repo, &subdir, &reason)),
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

fn materialize_and_judge(
    source: &crate::Source,
    repo: &RepoMeta,
    subdir: &str,
    manifest: &str,
) -> Result<Result<Facts, String>, Error> {
    let logical = logical_root(&repo.full_name, subdir);
    let temp = validate::scratch()?;
    let manifest_path = temp.path().join("phux-plugin.toml");
    fs::write(&manifest_path, manifest)
        .map_err(|err| Error::new(format!("writing a temporary manifest: {err}")))?;
    for relative in validate::theme_color_rels(manifest) {
        let remote = if subdir.is_empty() {
            relative.clone()
        } else {
            format!("{subdir}/{relative}")
        };
        let Some(body) = github::read_text(source, repo, &remote)? else {
            continue;
        };
        let destination = temp.path().join(&relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| Error::new(format!("writing a theme file: {err}")))?;
        }
        fs::write(&destination, body)
            .map_err(|err| Error::new(format!("writing a theme file: {err}")))?;
    }
    Ok(validate::judge(&manifest_path, &logical))
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

fn skip(repo: &RepoMeta, subdir: &str, reason: &str) -> Skipped {
    Skipped {
        repo: repo.full_name.clone(),
        subdir: subdir.to_owned(),
        reason: reason.to_owned(),
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
}
