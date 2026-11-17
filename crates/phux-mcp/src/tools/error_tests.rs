#[cfg(test)]
mod error_tests {
    use super::*;

    /// An old-protocol server stays on `transport`. The remedy names `kill`
    /// when the socket-owner pid is known; otherwise it asks to inspect that socket.
    #[test]
    fn an_incompatible_transport_error_names_the_stop_recovery() {
        let socket = Path::new("/tmp/old.sock");
        let err = AttachError::IncompatibleProtocol(
            "incompatible protocol: client offered 1.4.0, server requires 0.9.x; \
             update the phux server so protocol major.minor match"
                .to_owned(),
        );
        let with_pid = render_transport_error(&err, socket, "spawn", Some(4242));
        let doc = with_pid.document();
        assert_eq!(doc["error"]["code"], "transport");
        assert_eq!(doc["exit_code"], 1);
        let remedy = doc["remedy"].as_str().unwrap_or_default();
        assert!(remedy.contains("kill 4242"), "{remedy}");
        assert!(
            remedy.contains("`ps -p 4242 -o pid=,ppid=,user=,args=`"),
            "{remedy}"
        );
        assert!(
            remedy.contains("phux server --ensure --socket '/tmp/old.sock'`"),
            "{remedy}"
        );
        for forbidden in [
            "stale",
            "phux upgrade",
            "survive",
            "remove",
            "pkill",
            "killall",
        ] {
            assert!(!remedy.contains(forbidden), "{forbidden} in {remedy}");
        }

        let unknown = render_transport_error(&err, socket, "spawn", None);
        let unknown_doc = unknown.document();
        let remedy = unknown_doc["remedy"].as_str().unwrap_or_default();
        assert!(remedy.contains("`lsof -nP -- '/tmp/old.sock'`"), "{remedy}");
        assert!(!remedy.contains("`kill "), "{remedy}");
        for forbidden in [
            "stale",
            "phux upgrade",
            "survive",
            "remove",
            "pkill",
            "killall",
        ] {
            assert!(!remedy.contains(forbidden), "{forbidden} in {remedy}");
        }
    }

    /// MCP classifies a transport failure with the CLI's own refusal: the
    /// no-server remedy names `phux server --ensure` and the server log, and
    /// a socket that cannot be dialed names the path and the OS reason.
    #[test]
    fn a_transport_error_is_the_cli_refusal() {
        let socket = Path::new("/tmp/phux-mcp-test.sock");
        let refused = AttachError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let doc = render_transport_error(&refused, socket, "spawn", None).document();
        assert_eq!(doc["error"]["code"], "no_server");
        assert_eq!(doc["exit_code"], 1);
        let remedy = doc["remedy"].as_str().unwrap_or_default();
        assert!(remedy.contains("`phux server --ensure`"), "{remedy}");
        assert!(remedy.contains("server log: "), "{remedy}");
        assert_eq!(
            doc,
            phux_client::contract::error_document(
                &phux_client::contract::transport_refusal(&refused, socket, "spawn", None),
                1
            )
        );

        let denied = AttachError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let doc = render_transport_error(&denied, socket, "spawn", None).document();
        assert_eq!(doc["error"]["code"], "transport");
        let message = doc["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.starts_with("spawn: cannot reach the server socket /tmp/phux-mcp-test.sock: "),
            "{message}"
        );
    }
}
