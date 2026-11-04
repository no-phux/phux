use super::*;

#[tokio::test(flavor = "current_thread")]
async fn attach_negotiation_preserves_custom_caps_then_sends_attach() {
    let colors = TerminalDefaultColors {
        foreground: TerminalColor { r: 1, g: 2, b: 3 },
        background: TerminalColor { r: 4, g: 5, b: 6 },
    };
    let (mut client, mut server) = pair();
    let client_side = async {
        client
            .negotiate(
                attach_client_name(),
                attach_client_caps(Some(colors), &test_dial()),
            )
            .await
            .expect("HELLO_OK");
        client
            .send(&FrameKind::Attach {
                attach_id: 1,
                target: AttachTarget::Last,
                viewport: ViewportInfo::new(120, 40),
                request_scrollback: true,
                scrollback_limit_lines: 10_000,
                role_policy: None,
            })
            .await
            .expect("ATTACH");
    };
    let server_side = async {
        let hello = server.recv().await.expect("HELLO");
        let FrameKind::Hello { client_caps, .. } = &hello else {
            panic!("expected HELLO");
        };
        let (selected_profile, bootstrap_limits) =
            select_bootstrap_profile(client_caps, &BootstrapCapabilities::new())
                .expect("intersect");
        server
            .send(&FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new(),
                server_id: Vec::new(),
                selected_profile: Some(selected_profile),
                bootstrap_limits,
            })
            .await
            .expect("HELLO_OK");
        (hello, server.recv().await.expect("ATTACH"))
    };
    let ((), (hello, attach)) = tokio::join!(client_side, server_side);
    let FrameKind::Hello { client_caps, .. } = hello else {
        panic!("first frame must be HELLO");
    };
    assert_eq!(client_caps.default_colors, Some(colors));
    assert!(client_caps.layers.contains(Layer::L3));
    assert!(matches!(attach, FrameKind::Attach { .. }));
}
