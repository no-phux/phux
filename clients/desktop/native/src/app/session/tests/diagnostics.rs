//! Copy Diagnostics: the redacted receipt it puts on the clipboard.

use super::*;

#[gpui::test]
fn copy_diagnostics_puts_a_redacted_identity_receipt_on_the_clipboard(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-diagnostics-window-{}", std::process::id()));
    let main = desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                crate::app::AppOptions {
                    socket: path.join("isolated.sock"),
                    session: String::new(),
                    phux: path.join("not-installed-phux"),
                    layout_path: path.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: Some("Isolated window fixture".into()),
                    cli_install: None,
                },
                cx,
            )
            .unwrap()
    });
    cx.run_until_parked();
    main.update(cx, |_, window, cx| {
        window.dispatch_action(Box::new(Run::new("copy-diagnostics")), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let text = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("receipt on the clipboard");
    let value: serde_json::Value = serde_json::from_str(&text).expect("receipt is JSON");
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut expected = crate::diag::Receipt::FIELDS.to_vec();
    expected.sort_unstable();
    assert_eq!(keys, expected);
    assert_eq!(value["host"], "local");
    assert_eq!(value["appProtocol"], crate::updates::own_protocol());
    assert!(value["appVersion"].as_str().is_some_and(|v| !v.is_empty()));
    assert!(value["serverId"].is_null(), "no link, no server identity");
}

/// The linked branch of the receipt: identity comes from the connection and
/// the connection error is redacted on the way out.
#[gpui::test]
fn copy_diagnostics_reads_identity_from_the_linked_connection(cx: &mut TestAppContext) {
    use crate::app::desk::{Desk, Link};
    use crate::connection::Connection;
    use phux_client_runtime::control::{ControlPlane, Status};
    use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ServerCapabilities};
    use phux_protocol::wire::frame::FrameKind;

    const TOKEN: &str = "3f9c0a7be41d58c26a9f0e1b7d4c3a2958e6f1b0c9d8a7e6f5a4b3c2d1e0f9a8";
    let _dial = crate::connection::DeterministicDial::arm();
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-diagnostics-linked-{}", std::process::id()));
    let main = desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                crate::app::AppOptions {
                    socket: path.join("isolated.sock"),
                    session: String::new(),
                    phux: path.join("not-installed-phux"),
                    layout_path: path.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: Some("Isolated window fixture".into()),
                    cli_install: None,
                },
                cx,
            )
            .unwrap()
    });
    cx.run_until_parked();
    let desk = main
        .update(cx, |root, _, _| {
            root.view().clone().downcast::<Desk>().unwrap()
        })
        .unwrap();

    let mut connection =
        Connection::connect("/isolated-diagnostics.sock".into(), "work".into()).unwrap();
    let client = std::sync::Arc::clone(&connection.client);
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    let protocol = phux_protocol::PROTOCOL_VERSION;
    client
        .feed(FrameKind::HelloOk {
            protocol_major: protocol.major,
            protocol_minor: protocol.minor,
            protocol_patch: protocol.patch,
            server_caps: ServerCapabilities::new(),
            server_id: vec![0xAB; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    connection.drain();
    connection.status = Status::Failed;
    connection.error = Some(format!(
        "dial wss://me:hunter2@box:8787 refused token={TOKEN}"
    ));
    let epoch = connection.connection_epoch();

    desk.update(cx, |desk, cx| {
        let workspace = crate::workspace::Workspace::new(
            "work".into(),
            crate::workspace::WorkspaceOptions {
                fresh: false,
                initial_terminal: None,
            },
        );
        desk.link = Some(Link {
            connection,
            workspace,
        });
        desk.copy_diagnostics(cx);
    });
    let text = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("receipt on the clipboard");
    let value: serde_json::Value = serde_json::from_str(&text).expect("receipt is JSON");
    assert_eq!(value["serverId"], "ab".repeat(16));
    assert_eq!(value["connectionEpoch"], epoch);
    assert_eq!(
        value["serverProtocol"],
        format!("{}.{}.{}", protocol.major, protocol.minor, protocol.patch)
    );
    assert_eq!(value["status"], "Failed");
    assert_eq!(value["host"], "local");
    assert_eq!(
        value["connectionError"],
        "dial wss://<redacted>@box:8787 refused token=<redacted>"
    );
    assert!(!text.contains("hunter2") && !text.contains(TOKEN), "{text}");
}
