//! Start request validation and current/legacy idempotency digests.

use super::{Error, StartRequest, digest};

pub(super) fn start_digest(request: &StartRequest) -> Result<String, Error> {
    let mut payload = serde_json::json!({
            "agent_kind": &request.agent_kind,
            "alias": request.alias.display(),
            "branch": &request.branch,
            "expected_revision": request.expected_revision,
            "max_agents": request.max_agents,
            "model": &request.model,
            "repo_root": &request.repo_root,
            "worktree_path": &request.worktree_path,
    });
    if let Some(authority) = request.authority {
        payload["authority"] = serde_json::json!(authority.to_string());
    }
    digest("start", &payload)
}

pub(super) fn legacy_start_matches(request: &StartRequest, stored: &str) -> Result<bool, Error> {
    if request.authority.is_none() {
        return Ok(false);
    }
    let mut legacy = request.clone();
    legacy.authority = None;
    Ok(start_digest(&legacy)? == stored)
}

pub(super) fn validate_start(request: &StartRequest) -> Result<(), Error> {
    if !token_ok(&request.agent_kind, 64)
        || (!request.model.is_empty() && !token_ok(&request.model, 128))
    {
        return Err(Error::invalid_text(
            "agent",
            "must look like KIND or KIND/MODEL (letters, digits, `.`, `_`, `-`)",
        ));
    }
    if request.max_agents == 0 {
        return Err(Error::invalid_text("max_agents", "must be at least 1"));
    }
    if request.worktree_path.is_empty() || request.worktree_path.contains('\0') {
        return Err(Error::invalid_text("worktree", "must be a path"));
    }
    if request.branch.is_empty() || request.branch.contains('\0') || request.branch.len() > 256 {
        return Err(Error::invalid_text("branch", "must be a branch name"));
    }
    if let Some(binding) = &request.launch
        && (binding.session.is_empty()
            || binding.worktree_path != request.worktree_path
            || binding.branch != request.branch)
    {
        return Err(Error::invalid_text("binding", "is incomplete"));
    }
    Ok(())
}

fn token_ok(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}
