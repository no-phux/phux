//! Explicit source startup, using the existing supervised terminal argv path.
use super::desk::{Desk, Modal};
use super::notifications::Kind;
use gpui::{Context, Window};
use phux_client_runtime::control::SpawnRequest;
use phux_protocol::caps::ServerFeatureExt;

impl Desk {
    /// Reuse the CLI this desktop already resolved; never bootstrap or search
    /// the producer's PATH for a different executable.
    fn mirror_cli(&self) -> Option<&std::path::Path> {
        let path = if self.phux == std::path::Path::new("phux") {
            &self.cli_install.as_ref()?.path
        } else {
            &self.phux
        };
        (path.is_absolute() && path.is_file()).then_some(path.as_path())
    }

    pub(crate) fn open_resource_mirror(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let source = match id {
            "mirror-page" => "page",
            "mirror-tree" => "tree",
            "mirror-surface" => "surface",
            _ => return false,
        };
        if let Some(reason) = self.mirror_refusal(source) {
            self.post(Kind::Refusal, "Could not start mirror", Some(reason));
            cx.notify();
        } else {
            self.open_modal(Modal::Mirror { source }, window, cx);
        }
        true
    }

    fn mirror_refusal(&self, source: &str) -> Option<String> {
        self.mirror_host_refusal(source)
            .or_else(|| self.mirror_server_refusal(source))
    }

    fn mirror_host_refusal(&self, source: &str) -> Option<String> {
        if self.remote.is_some() {
            return Some("Mirror startup is local-only. Open a window on the executing machine; phux does not migrate source processes between hosts.".into());
        }
        if source != "page" && !cfg!(target_os = "macos") {
            return Some("Application and display mirrors require macOS.".into());
        }
        if self.mirror_cli().is_none() {
            return Some(
                "Select an explicit installed or checkout-built phux CLI before starting a mirror."
                    .into(),
            );
        }
        None
    }

    fn mirror_server_refusal(&self, source: &str) -> Option<String> {
        let Some(connection) = self.link.as_ref().map(|link| &link.connection) else {
            return Some("Connect to a local session before starting a mirror.".into());
        };
        if connection.status != phux_client_runtime::control::Status::Attached {
            return Some("Wait for the local session to connect before starting a mirror.".into());
        }
        let Some(server) = connection.server.as_ref() else {
            return Some("The local server has not completed negotiation.".into());
        };
        let supported = match source {
            "tree" => server.has_ext(ServerFeatureExt::AccessibilityTree),
            "surface" => server.has_ext(ServerFeatureExt::PixelSurface),
            _ => {
                server.has_ext(ServerFeatureExt::AccessibilityTree)
                    && server.has_ext(ServerFeatureExt::PixelSurface)
            }
        };
        (!supported).then(|| "This server does not support the requested resource kind. Update it before starting a mirror.".into())
    }

    pub(crate) fn start_resource_mirror(
        &mut self,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Modal::Mirror { source } = self.modal else {
            return;
        };
        if let Some(reason) = self.mirror_refusal(source) {
            self.post(Kind::Refusal, "Could not start mirror", Some(reason));
            return;
        }
        let Some(phux) = self.mirror_cli() else {
            return;
        };
        let command = match mirror_argv(phux, &self.socket, source, &target) {
            Ok(command) => command,
            Err(reason) => {
                self.post(Kind::Refusal, "Could not start mirror", Some(reason));
                return;
            }
        };
        let Some(link) = self.link.as_mut() else {
            return;
        };
        let request = SpawnRequest {
            command: Some(command),
            session_id: link.connection.home_session(),
            ..SpawnRequest::default()
        };
        match link.workspace.launch_setup(&link.connection, request) {
            Ok(_) => self.close_modal(window, cx),
            Err(error) => self.post(
                Kind::Error,
                "Could not start mirror",
                Some(error.to_string()),
            ),
        }
        cx.notify();
    }
}

fn mirror_argv(
    phux: &std::path::Path,
    socket: &std::path::Path,
    source: &str,
    target: &str,
) -> Result<Vec<String>, String> {
    let target = target.trim();
    if target.is_empty() || target.len() > 4096 || target.contains(['\0', '\r', '\n']) {
        return Err("Enter a source name without line breaks (at most 4096 bytes).".into());
    }
    if !matches!(source, "page" | "tree" | "surface") {
        return Err("Unsupported mirror source.".into());
    }
    if target.starts_with('-') {
        return Err("Use a source name, not a command-line option.".into());
    }
    Ok(vec![
        phux.display().to_string(),
        "--socket".into(),
        socket.display().to_string(),
        "agent".into(),
        source.into(),
        "mirror".into(),
        target.into(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn producer_argv_is_explicit_and_never_shell_interpreted() {
        let argv = mirror_argv(
            std::path::Path::new("/checkout/phux"),
            std::path::Path::new("/isolated.sock"),
            "page",
            "https://example.test/; touch nope",
        )
        .unwrap();
        assert_eq!(
            argv,
            [
                "/checkout/phux",
                "--socket",
                "/isolated.sock",
                "agent",
                "page",
                "mirror",
                "https://example.test/; touch nope"
            ]
        );
        for target in ["", "--pane @1", "one\ntwo", "\0"] {
            assert!(
                mirror_argv(
                    std::path::Path::new("/checkout/phux"),
                    std::path::Path::new("/isolated.sock"),
                    "tree",
                    target
                )
                .is_err()
            );
        }
    }

    #[gpui::test]
    fn startup_uses_a_dedicated_supervised_argv_and_refuses_remote_early(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::AppContext as _;
        use phux_protocol::wire::frame::FrameKind;
        let _dial = crate::connection::DeterministicDial::arm();
        let (handle, desk) = crate::app::overlays::palette_tests::fixture(cx);
        let connection = crate::workspace::view_tests::documents::connection();
        let mut workspace = crate::workspace::Workspace::new("docs".into(), Default::default());
        workspace.settle(&connection);
        desk.update(cx, |desk, _| {
            desk.link = Some(crate::app::desk::Link {
                connection,
                workspace,
            });
            desk.phux = std::env::current_exe().unwrap();
            desk.socket = "/isolated-source.sock".into();
        });
        cx.update_window(handle, |_, window, cx| {
            desk.update(cx, |desk, cx| {
                let _ = desk.client().unwrap().take_outbound();
                assert!(desk.open_resource_mirror("mirror-page", window, cx));
                assert_eq!(desk.modal, Modal::Mirror { source: "page" });
                desk.start_resource_mirror("first".into(), window, cx);
                let frames: Vec<_> = desk
                    .client()
                    .unwrap()
                    .take_outbound()
                    .iter()
                    .map(|bytes| FrameKind::decode(bytes).unwrap().0)
                    .collect();
                assert_eq!(
                    frames.len(),
                    1,
                    "one spawn, not typing into a shell: {frames:?}"
                );
                let FrameKind::SpawnResource { ref command, .. } = frames[0] else {
                    panic!("expected supervised spawn: {frames:?}")
                };
                let argv = command.as_ref().unwrap();
                assert_eq!(
                    &argv[1..],
                    [
                        "--socket",
                        "/isolated-source.sock",
                        "agent",
                        "page",
                        "mirror",
                        "first"
                    ]
                );
                desk.remote = Some("other-host".into());
                assert!(desk.open_resource_mirror("mirror-page", window, cx));
                assert_eq!(desk.modal, Modal::None);
                assert!(desk.client().unwrap().take_outbound().is_empty());
            })
        })
        .unwrap();
    }
}
