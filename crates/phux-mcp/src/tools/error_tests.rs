#[cfg(test)]
mod error_tests {
    use super::*;

    /// MCP's transport refusal is the CLI's own document, exit code included.
    #[test]
    fn a_transport_error_is_the_cli_refusal() {
        let socket = Path::new("/tmp/phux-mcp-test.sock");
        let refused = AttachError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let doc = render_transport_error(&refused, socket, "spawn", None).document();
        assert_eq!(doc["error"]["code"], "no_server");
        assert_eq!(
            doc,
            phux_client::contract::error_document(
                &phux_client::contract::transport_refusal(&refused, socket, "spawn", None),
                1
            )
        );
    }
}
