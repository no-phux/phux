//! Rune as ssh's `SSH_ASKPASS`: a host that wants a password, a 2FA code
//! or a key tap asks in a dialog, since Rune has no terminal for ssh.
//!
//! The runtime runs ssh with this executable as its askpass program and
//! [`ASKPASS_ENV`] set; ssh runs it once per prompt, with the prompt as the
//! only argument, and reads the answer from stdout. The answer is never
//! stored or logged.

use std::path::PathBuf;

use phux_client_runtime::connection::ASKPASS_ENV;

/// The askpass program for the connection the person chose: Rune itself on
/// macOS, the desktop's own `SSH_ASKPASS` elsewhere, or none, so ssh never
/// prompts.
pub(crate) fn program() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return std::env::current_exe().ok();
    }
    std::env::var_os("SSH_ASKPASS")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// When ssh ran Rune as its askpass program, answer the prompt and give
/// the exit code; otherwise `None`, and Rune starts as usual.
pub(crate) fn answer() -> Option<i32> {
    if std::env::var_os(ASKPASS_ENV).is_none_or(|marker| marker != "1") {
        return None;
    }
    let prompt = std::env::args().nth(1).unwrap_or_default();
    Some(match ask(&prompt) {
        Some(answer) => {
            println!("{answer}");
            0
        }
        // Cancelled: ssh counts the prompt as failed.
        None => 1,
    })
}

#[cfg(target_os = "macos")]
fn ask(prompt: &str) -> Option<String> {
    // The prompt reaches AppleScript as an argument, never as script text.
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &dialog_script(prompt)])
        .arg(prompt)
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end_matches('\n')
            .to_owned()
    })
}

#[cfg(not(target_os = "macos"))]
const fn ask(_prompt: &str) -> Option<String> {
    None
}

/// The dialog for `prompt`: a hidden field, except for ssh's yes/no
/// questions (a new host key), whose answer is not a secret.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only macOS shows a dialog")
)]
fn dialog_script(prompt: &str) -> String {
    let hidden = if prompt.contains("(yes/no") {
        ""
    } else {
        " with hidden answer"
    };
    format!(
        "on run argv\n\
         activate\n\
         display dialog (item 1 of argv) default answer \"\"{hidden} with title \"Rune\" with icon caution\n\
         return text returned of result\n\
         end run"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_hidden_and_the_prompt_is_never_script() {
        let password = dialog_script("me@box's password: \" & do shell script \"x");
        assert!(password.contains("with hidden answer"), "{password}");
        assert!(!password.contains("do shell script"), "{password}");
        let host_key =
            dialog_script("Are you sure you want to continue connecting (yes/no/[fingerprint])?");
        assert!(!host_key.contains("hidden"), "{host_key}");
    }
}
