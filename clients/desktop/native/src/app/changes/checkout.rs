//! Blocking local-checkout reads, kept separate from the panel's UI state.
use super::*;

/// Status, then both comparisons, then the text of each conflicted file.
pub(super) fn read_checkout(
    folder: &std::path::Path,
) -> Result<Option<(Status, Comparisons)>, String> {
    let Some(status) = git::read_status(folder)? else {
        return Ok(None);
    };
    let root = status.root.clone();
    let untracked: Vec<String> = status
        .in_area(Area::Untracked)
        .map(|entry| entry.path.clone())
        .collect();
    let conflicts: Vec<(String, String)> = status
        .in_area(Area::Conflicted)
        .filter_map(|entry| {
            git::read_file(&root, &entry.path)
                .ok()
                .map(|text| (entry.path.clone(), text))
        })
        .collect();
    let changes = git::read_patch(&root, Source::Changes, &untracked, review::FETCH_CONTEXT)?;
    let staged = git::read_patch(&root, Source::Staged, &[], review::FETCH_CONTEXT)?;
    let comparisons = Comparisons {
        changes: Loaded::parse(changes, conflicts),
        staged: Loaded::parse(staged, Vec::new()),
    };
    Ok(Some((status, comparisons)))
}

/// `~/x` as an absolute path, for a shell that reported its folder that way.
pub(super) fn expand_home(cwd: &str) -> PathBuf {
    match (cwd.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            PathBuf::from(home).join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(cwd),
    }
}
