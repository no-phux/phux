//! A host key ssh refused, and the fix to offer for it.

use super::shell_quote;

/// What ssh prints when it refuses a host key: a changed or revoked one, or
/// under `BatchMode` one it has never seen.
pub const HOST_KEY_REFUSED: &str = "Host key verification failed";

/// The fix for ssh refusing `destination`'s host key, read from ssh's
/// stderr, or `None` when that is not why it failed.
///
/// phux never removes or accepts a key itself: a changed key is what a
/// reinstall looks like, and also what an interception looks like.
#[must_use]
pub fn host_key_remedy(stderr: &str, destination: &str) -> Option<String> {
    if !stderr.contains(HOST_KEY_REFUSED) {
        return None;
    }
    let check =
        format!("connect once with `ssh {destination}` to check and accept the key it shows");
    if stderr.contains("REVOKED HOST KEY") {
        return Some(format!(
            "{destination}'s host key has been revoked; ask whoever runs the host before connecting"
        ));
    }
    if !(stderr.contains("IDENTIFICATION HAS CHANGED") || stderr.contains("DNS SPOOFING")) {
        return Some(format!(
            "ssh does not know {destination}'s host key yet, and phux never accepts one on its \
             own: {check}"
        ));
    }
    Some(format!(
        "{destination} presented a different host key than the one you trusted. A reinstall \
         does that, and so does someone intercepting the connection, so confirm the new key \
         with whoever runs the host; then remove the old key with {} and {check}",
        remove_command(stderr)
    ))
}

/// The `ssh-keygen -R` that removes the key ssh refused, quoted in
/// backticks. Some builds print it under "remove with:"; every build names
/// the `known_hosts` file (`Offending ... key in FILE:LINE`) and the entry
/// (`[host]:port`, a `HostKeyAlias`) whose key changed.
fn remove_command(stderr: &str) -> String {
    let lines = || stderr.lines().map(str::trim);
    if let Some(line) = lines().find(|line| line.starts_with("ssh-keygen ")) {
        return format!("`{line}`");
    }
    let entry = lines().find_map(|line| {
        line.strip_prefix("Host key for ")?
            .split_once(" has changed")
            .map(|(entry, _)| entry)
    });
    let file = lines().find_map(|line| {
        let (_, rest) = line.strip_prefix("Offending ")?.split_once(" key in ")?;
        rest.rsplit_once(':').map(|(file, _)| file)
    });
    match (file, entry) {
        (Some(file), Some(entry)) => format!(
            "`ssh-keygen -f {} -R {}`",
            shell_quote(file),
            shell_quote(entry)
        ),
        (None, Some(entry)) => format!("`ssh-keygen -R {}`", shell_quote(entry)),
        _ => "`ssh-keygen -R` and the host's name".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_host_key_gets_a_fix_and_never_an_automatic_one() {
        const CHANGED: &str = "\
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
Offending ED25519 key in /home/me/.ssh/known_hosts:3
  remove with:
  ssh-keygen -f '/home/me/.ssh/known_hosts' -R '[box]:2222'
Host key for [box]:2222 has changed and you have requested strict checking.
Host key verification failed.";
        // macOS's ssh prints no "remove with:"; the command is built from
        // the file and entry it does name.
        const CHANGED_MACOS: &str = "\
@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @
Offending ED25519 key in /Users/me/.ssh/known_hosts:1
Host key for [127.0.0.1]:22422 has changed and you have requested strict checking.
Host key verification failed.";
        let fix = host_key_remedy(CHANGED, "me@box").expect("changed");
        assert!(
            fix.contains("`ssh-keygen -f '/home/me/.ssh/known_hosts' -R '[box]:2222'`"),
            "ssh's own entry is quoted exactly: {fix}"
        );
        assert!(
            fix.contains("confirm the new key") && fix.contains("`ssh me@box`"),
            "{fix}"
        );

        let fix = host_key_remedy(CHANGED_MACOS, "box").expect("changed");
        assert!(
            fix.contains("`ssh-keygen -f '/Users/me/.ssh/known_hosts' -R '[127.0.0.1]:22422'`"),
            "{fix}"
        );

        let fix = host_key_remedy("Host key verification failed.\r\n", "box").expect("unknown");
        assert!(
            fix.contains("does not know") && fix.contains("`ssh box`"),
            "{fix}"
        );
        assert!(!fix.contains("ssh-keygen"), "nothing to remove: {fix}");

        let revoked = "WARNING: REVOKED HOST KEY DETECTED!\nHost key verification failed.";
        let fix = host_key_remedy(revoked, "box").expect("revoked");
        assert!(
            fix.contains("revoked") && !fix.contains("ssh-keygen"),
            "{fix}"
        );

        assert_eq!(
            host_key_remedy("Permission denied (publickey).", "box"),
            None
        );
    }
}
