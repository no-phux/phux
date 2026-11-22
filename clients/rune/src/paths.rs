//! Paths on the pane's host, found by the server, and the one shell word a
//! picked path becomes at the prompt.

/// `path` as one POSIX-shell word for a prompt standing in `origin`: relative
/// when it lies under `origin`, bare when every character is safe, otherwise
/// single-quoted. A relative word that would read as an option gets `./`.
/// `None` for anything but an absolute path free of control characters.
pub fn shell_word(path: &str, origin: &str) -> Option<String> {
    if !path.starts_with('/') || path.chars().any(char::is_control) {
        return None;
    }
    let word = match relative(path, origin) {
        Some(rest) if rest.starts_with('-') => format!("./{rest}"),
        Some(rest) => rest.to_owned(),
        None => path.to_owned(),
    };
    let safe = |c: char| c.is_ascii_alphanumeric() || "_-./+@%=:,".contains(c);
    Some(if word.chars().all(safe) {
        word
    } else {
        format!("'{}'", word.replace('\'', r#"'"'"'"#))
    })
}

/// What `path` is called below `origin`, if it is below it.
pub fn relative<'a>(path: &'a str, origin: &str) -> Option<&'a str> {
    origin
        .starts_with('/')
        .then(|| path.strip_prefix(origin.trim_end_matches('/')))
        .flatten()
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_relative_minimal_and_never_options() {
        assert_eq!(
            shell_word("/repo/Sources/App.swift", "/repo/").as_deref(),
            Some("Sources/App.swift")
        );
        assert_eq!(shell_word("/repo/-rf", "/repo").as_deref(), Some("./-rf"));
        assert_eq!(
            shell_word("/elsewhere/a", "/repo").as_deref(),
            Some("/elsewhere/a")
        );
        assert_eq!(
            shell_word("/repository/a", "/repo").as_deref(),
            Some("/repository/a")
        );
        assert_eq!(shell_word("/repo", "/repo").as_deref(), Some("/repo"));
        assert_eq!(shell_word("/repo/a b", "~").as_deref(), Some("'/repo/a b'"));
        assert_eq!(
            shell_word("/repo/it's", "/repo").as_deref(),
            Some(r#"'it'"'"'s'"#)
        );
        assert!(shell_word("/repo/a\nrm", "/repo").is_none());
        assert!(shell_word("relative", "/repo").is_none());
    }
}
