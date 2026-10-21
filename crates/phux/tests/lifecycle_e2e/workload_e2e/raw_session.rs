/// One raw QUIC connection to the far listener, dialed the way the registry
/// entry says (its token, pin, and enrolled certificate), with HELLO sent.
struct RawSession {
    runtime: tokio::runtime::Runtime,
    connection: phux_dial::quic::QuicConnection,
}

impl RawSession {
    fn open(entry: &toml::Value, port: u16) -> Self {
        use phux_protocol::wire::frame::FrameKind;

        let token = std::fs::read_to_string(entry_path(entry, "token-file")).expect("token");
        let dial = phux_dial::quic::QuicDial {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            server_name: "localhost".to_owned(),
            token: Some(phux_dial::quic::parse_token_hex(token.trim()).expect("hex token")),
            trust: phux_dial::CertTrust::Pinned(
                entry["cert-fingerprint"].as_str().expect("pin").to_owned(),
            ),
            identity: Some(phux_dial::TlsClientIdentity::PemFiles {
                certificate: entry_path(entry, "client-cert"),
                private_key: entry_path(entry, "client-key"),
            }),
            inner: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut connection = runtime
            .block_on(phux_dial::quic::dial(&dial))
            .expect("the TLS handshake completes on the client side");
        let hello = FrameKind::Hello {
            client_name: "enrollment-e2e".to_owned(),
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            client_caps: phux_protocol::caps::ClientCapabilities::default(),
        };
        let mut encoded = bytes::BytesMut::new();
        hello.encode(&mut encoded);
        // A refused connection may already be closing; the reads say so.
        let _ = runtime.block_on(connection.2.write_all(&encoded));
        Self {
            runtime,
            connection,
        }
    }

    /// The next frame, or `None` once the server closed the stream.
    fn next_frame(&mut self) -> Option<phux_protocol::wire::frame::FrameKind> {
        let recv = &mut self.connection.3;
        self.runtime.block_on(async {
            let mut header = [0_u8; 4];
            let read = tokio::time::timeout(READY_DEADLINE, recv.read_exact(&mut header)).await;
            let Ok(Ok(())) = read else { return None };
            let len = u32::from_be_bytes(header) as usize;
            let mut packet = header.to_vec();
            packet.resize(4 + len, 0);
            recv.read_exact(&mut packet[4..]).await.ok()?;
            let (frame, rest) =
                phux_protocol::wire::frame::FrameKind::decode(&packet).expect("decode frame");
            assert!(rest.is_empty());
            Some(frame)
        })
    }

    /// Every frame until the server closes.
    fn frames_until_close(&mut self) -> Vec<phux_protocol::wire::frame::FrameKind> {
        std::iter::from_fn(|| self.next_frame()).collect()
    }

    /// Read until `HELLO_OK`, failing if the server closes first.
    fn await_hello_ok(&mut self) {
        use phux_protocol::wire::frame::FrameKind;
        let mut seen = Vec::new();
        let admitted = std::iter::from_fn(|| self.next_frame())
            .inspect(|frame| seen.push(frame.clone()))
            .any(|frame| matches!(frame, FrameKind::HelloOk { .. }));
        assert!(
            admitted,
            "the enrolled certificate was not admitted: {seen:?}"
        );
    }

    /// Why the connection closed, as quinn reports it.
    fn close_reason(&self) -> String {
        format!("{:?}", self.connection.1.close_reason())
    }

    /// A loopback dial that presents `cert`/`key` and no bearer. A loopback
    /// paired listener admits the certificate alone.
    fn open_cert(port: u16, cert: &Path, key: &Path) -> Self {
        use phux_protocol::wire::frame::FrameKind;

        let dial = phux_dial::quic::QuicDial {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            server_name: "localhost".to_owned(),
            token: None,
            trust: phux_dial::CertTrust::SkipVerify,
            identity: Some(phux_dial::TlsClientIdentity::PemFiles {
                certificate: cert.to_owned(),
                private_key: key.to_owned(),
            }),
            inner: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut connection = runtime
            .block_on(phux_dial::quic::dial(&dial))
            .expect("the TLS handshake completes on the client side");
        let hello = FrameKind::Hello {
            client_name: "observe-host-e2e".to_owned(),
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            client_caps: phux_protocol::caps::ClientCapabilities::default(),
        };
        let mut encoded = bytes::BytesMut::new();
        hello.encode(&mut encoded);
        // As in `open`, a refused connection may close before HELLO writes;
        // the following reads and close reason establish the refusal.
        let _ = runtime.block_on(connection.2.write_all(&encoded));
        Self {
            runtime,
            connection,
        }
    }

    fn write_frame(
        runtime: &tokio::runtime::Runtime,
        send: &mut (impl tokio::io::AsyncWrite + Unpin),
        frame: &phux_protocol::wire::frame::FrameKind,
    ) {
        use tokio::io::AsyncWriteExt;
        let mut encoded = bytes::BytesMut::new();
        frame.encode(&mut encoded);
        runtime
            .block_on(send.write_all(&encoded))
            .expect("write frame");
    }

    fn send_frame(&mut self, frame: &phux_protocol::wire::frame::FrameKind) {
        let Self {
            runtime,
            connection,
        } = self;
        Self::write_frame(runtime, &mut connection.2, frame);
    }

    /// The `COMMAND_RESULT` for `request_id`, failing if the server closes first.
    fn await_command_result(
        &mut self,
        request_id: u32,
    ) -> phux_protocol::wire::frame::CommandResult {
        use phux_protocol::wire::frame::FrameKind;
        let mut seen = Vec::new();
        let found = std::iter::from_fn(|| self.next_frame()).find_map(|frame| {
            let hit = match &frame {
                FrameKind::CommandResult {
                    request_id: id,
                    result,
                } if *id == request_id => Some(result.clone()),
                _ => None,
            };
            seen.push(frame);
            hit
        });
        assert!(found.is_some(), "no command result {request_id}: {seen:?}");
        found.expect("checked above")
    }
}
