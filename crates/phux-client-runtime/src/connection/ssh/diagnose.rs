//! What a failed ssh run means, worded for the person, with a command to
//! see more when ssh said too little.

use std::process::Stdio;

use super::{pre_auth_drop, program, shell_quote};

/// What a failed ssh login to `destination` most likely means.
///
/// Read from ssh's stderr, when phux knows more than ssh said: sshd's `MaxStartups`
/// dropping it, its `MaxSessions` refusing one more session on phux's
/// master, or a `BatchMode yes` in the user's config refusing to prompt for
/// a password or code.
#[must_use]
pub fn failure_hint(stderr: &str, destination: &str) -> Option<String> {
    hint(stderr, destination, || batch_mode_configured(destination))
}

fn hint(stderr: &str, destination: &str, batch_mode: impl FnOnce() -> bool) -> Option<String> {
    if pre_auth_drop(stderr) {
        return Some(format!(
            "{destination}'s sshd closed the connection before the login began, which is what \
             its MaxStartups limit does when many ssh connections open at once; phux retried a \
             few times. Connect again in a moment, open fewer at once, or raise MaxStartups in \
             the host's sshd_config"
        ));
    }
    if stderr.contains("Session open refused by peer") {
        return Some(format!(
            "{destination}'s sshd refused another session on phux's shared ssh connection, \
             which is what its MaxSessions limit (10 by default) does; close some, or raise \
             MaxSessions in the host's sshd_config"
        ));
    }
    (stderr.contains("Permission denied") && batch_mode()).then(|| {
        format!(
            "your ssh config sets BatchMode yes for {destination}, so ssh asks for no password, \
             code or key tap, and phux cannot either; use a key that works without a prompt, \
             or remove BatchMode from that Host block"
        )
    })
}

/// Whether the user's ssh config (`ssh -G`) turns on `BatchMode` for
/// `destination`.
fn batch_mode_configured(destination: &str) -> bool {
    std::process::Command::new(program())
        .args(["-G", "--", destination])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| batch_mode_in(&String::from_utf8_lossy(&output.stdout)))
}

fn batch_mode_in(ssh_g: &str) -> bool {
    ssh_g
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("batchmode yes"))
}

/// Whether ssh's stderr gives nothing to act on: empty, or only that the
/// connection closed.
fn opaque(stderr: &str) -> bool {
    stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .all(|line| {
            pre_auth_drop(line)
                || line.contains("Connection closed")
                || (line.starts_with("Connection to ") && line.ends_with(" closed."))
        })
}

/// A failed lane's stderr as the person sees it: ssh's own words verbatim,
/// what they mean when phux knows ([`failure_hint`]), and, when ssh said
/// too little, `diagnostic`: the same ssh run with `-v`.
pub(super) fn explain(stderr: &str, destination: &str, diagnostic: &str) -> String {
    let mut text = if stderr.is_empty() {
        "ssh printed nothing".to_owned()
    } else {
        stderr.to_owned()
    };
    if let Some(hint) = failure_hint(stderr, destination) {
        text = format!("{text}\n{hint}");
    }
    if opaque(stderr) {
        text = format!("{text}\nto see why, run: {diagnostic}");
    }
    text
}

/// `PROGRAM -v OPTIONS -- DESTINATION true`, quoted for a shell: the run
/// phux made, with ssh saying what it does, and nothing run on the host.
pub(super) fn diagnostic_command(options: &[String], destination: &str) -> String {
    let program = program();
    std::iter::once(program.to_string_lossy().as_ref())
        .chain(["-v"])
        .chain(options.iter().map(String::as_str))
        .chain(["--", destination, "true"])
        .map(shell_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c));
    if plain {
        word.to_owned()
    } else {
        shell_quote(word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_that_says_little_gets_stderr_verbatim_and_a_verbose_command() {
        let diagnostic = diagnostic_command(
            &[
                "-o".to_owned(),
                "ControlPath=/tmp/phux-ssh-501/%C".to_owned(),
            ],
            "me@my box",
        );
        assert!(
            diagnostic.ends_with(" -v -o ControlPath=/tmp/phux-ssh-501/%C -- 'me@my box' true"),
            "{diagnostic}"
        );

        let said = explain("", "box", &diagnostic);
        assert!(said.starts_with("ssh printed nothing\n"), "{said}");
        assert!(
            said.contains(&format!("to see why, run: {diagnostic}")),
            "{said}"
        );

        let closed = "Connection closed by 10.0.0.5 port 22";
        let said = explain(closed, "box", &diagnostic);
        assert!(said.starts_with(closed), "verbatim: {said}");
        assert!(said.contains("MaxStartups"), "{said}");
        assert!(said.contains("to see why, run:"), "{said}");

        // ssh already said what to do: no command to run.
        let refused = "ssh: connect to host box port 22: Connection refused";
        let said = explain(refused, "box", &diagnostic);
        assert_eq!(said, refused);
    }

    /// `BatchMode yes` in the user's config is why a password host refuses
    /// phux's prompt, so a refused login names it; nothing else does.
    #[test]
    fn a_refused_login_under_batch_mode_says_so() {
        assert!(batch_mode_in("user me\nbatchmode yes\nport 22\n"));
        assert!(!batch_mode_in("user me\nbatchmode no\n"));
        let denied = "me@vm: Permission denied (publickey,password).";
        let said = hint(denied, "vm", || true).expect("named");
        assert!(said.contains("BatchMode yes for vm"), "{said}");
        assert_eq!(hint(denied, "vm", || false), None);
        let full =
            "mux_client_request_session: session request failed: Session open refused by peer";
        assert!(hint(full, "vm", || false).is_some_and(|said| said.contains("MaxSessions")));
        let unreachable = "ssh: connect to host vm port 22: Connection refused";
        assert_eq!(hint(unreachable, "vm", || panic!("not asked")), None);
    }

    /// A host that takes the connection and closes it says next to nothing,
    /// so the lane's error carries ssh's words and a `-v` run to see why.
    /// Real ssh against a listener that hangs up; skipped without ssh.
    #[tokio::test]
    async fn a_lane_closed_before_login_says_what_ssh_said_and_how_to_see_more() {
        if std::process::Command::new("ssh")
            .arg("-V")
            .output()
            .is_err()
        {
            // No ssh to test with.
            return;
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });
        let destination = format!("ssh://127.0.0.1:{port}");
        let mut lane = super::super::establish(&destination, "phux")
            .await
            .expect("ssh starts");
        let said = lane
            .read_frames()
            .await
            .expect_err("closed before any frame")
            .describe("box");
        // Closed or reset, depending on how fast the hang-up lands.
        assert!(said.contains("127.0.0.1"), "verbatim: {said}");
        assert!(said.contains("MaxStartups"), "{said}");
        assert!(said.contains("to see why, run: "), "{said}");
        assert!(said.contains(" -v "), "{said}");
        assert!(said.contains(&format!("-- {destination} true")), "{said}");
    }
}
