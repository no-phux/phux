//! Raw newline-delimited JSON peers deliberately do not use SDK serialization.
//! This checks the actual stable V1 wire contract, including Cursor extensions.
use super::*;
use phux_protocol::ids::ResourceId;
use serde_json::json;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};

struct Peer {
    io: BufReader<DuplexStream>,
    callback: Arc<Callback>,
    commands: mpsc::Sender<NativeCommand>,
    shutdown: watch::Sender<bool>,
}
impl Peer {
    async fn send(&mut self, message: Value) {
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        self.io.get_mut().write_all(&bytes).await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(self.io.read_line(&mut line).await.unwrap(), 0);
        let message: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(message["jsonrpc"], "2.0");
        message
    }

    async fn respond(&mut self, request: &Value, result: Value) {
        self.send(json!({"jsonrpc":"2.0", "id":request["id"], "result":result}))
            .await;
    }

    async fn command(&self, id: u64, intent: NativeIntent) {
        self.commands
            .send(NativeCommand {
                operation_id: id,
                activation: "a".into(),
                intent,
            })
            .await
            .unwrap();
    }

    async fn answer(&self, id: u64, question_id: &str, response: Value) {
        self.command(
            id,
            NativeIntent::Answer {
                question_id: question_id.into(),
                response,
            },
        )
        .await;
    }

    async fn receipt(&self, id: u64, disposition: NativeDisposition) {
        wait_for(|| {
            self.callback
                .sink
                .0
                .backlog
                .lock()
                .rows
                .iter()
                .any(|(output, _)| {
                    matches!(output, crate::session::Output::Result(result)
                    if result.operation_id == id && result.disposition == disposition)
                })
        })
        .await;
    }

    async fn initialize(&mut self, load_session: bool) {
        let request = self.recv().await;
        assert_eq!(request["method"], "initialize");
        assert_eq!(request["params"]["protocolVersion"], 1);
        assert_eq!(request["params"]["clientInfo"]["name"], "phux-agent-host");
        let caps = &request["params"]["clientCapabilities"];
        assert_ne!(caps["fs"]["readTextFile"], true);
        assert_ne!(caps["fs"]["writeTextFile"], true);
        assert_ne!(caps["terminal"], true);
        assert!(caps["elicitation"].is_null());
        self.respond(&request, json!({
            "protocolVersion":1, "agentCapabilities":{"loadSession":load_session}, "authMethods":[]
        })).await;
    }

    async fn new_session(&mut self) {
        self.initialize(false).await;
        let request = self.recv().await;
        assert_eq!(request["method"], "session/new");
        assert_eq!(request["params"]["cwd"], "/tmp");
        assert_eq!(request["params"]["mcpServers"], json!([]));
        self.respond(&request, json!({"sessionId":"session"})).await;
        wait_for(|| self.callback.sink.view().provider_session_id.as_deref() == Some("session"))
            .await;
    }

    async fn stop(&mut self) {
        self.shutdown.send(true).unwrap();
        self.io.read_to_end(&mut Vec::new()).await.unwrap();
    }

    async fn permission(&mut self, id: &str, session: &str) {
        self.send(
            json!({"jsonrpc":"2.0", "id":id, "method":"session/request_permission", "params":{
                "sessionId":session, "toolCall":{"toolCallId":"tool", "title":"Tool permission"},
                "options":[{"optionId":"allow-once", "name":"Allow once", "kind":"allow_once"}]
            }}),
        )
        .await;
    }

    async fn question(&self, id: &str) {
        wait_for(|| self.callback.waiting.lock().contains_key(id)).await;
    }
}

async fn wait_for(mut predicate: impl FnMut() -> bool) {
    while !predicate() {
        tokio::task::yield_now().await;
    }
}

fn spec(native_id: Option<&str>) -> HostSpec {
    HostSpec {
        backend: "acp".into(),
        provider: "cursor".into(),
        command: Vec::new(),
        cwd: "/tmp".into(),
        env: BTreeMap::new(),
        socket: "/tmp/phux-acp-wire-test-no-socket".into(),
        parent: ResourceId::local(1),
        agent: ResourceId::local(2),
        activation: "a".into(),
        binding_proof: String::new(),
        native_id: native_id.map(str::to_owned),
    }
}

async fn run_peer(spec: HostSpec, test: impl AsyncFnOnce(Peer)) -> sdk::Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (host, peer) = tokio::io::duplex(64 * 1024);
        let (incoming, outgoing) = tokio::io::split(host);
        let callback = Arc::new(Callback {
            sink: crate::session::tests::sink(),
            provider: spec.provider.clone(),
            waiting: Mutex::new(BTreeMap::new()),
            rows: Mutex::new(Rows::default()),
        });
        let (commands, mut receive) = mpsc::channel(16);
        let (shutdown, mut stopped) = watch::channel(false);
        let driver = client_builder(callback.clone()).connect_with(
            acp::ByteStreams::new(outgoing.compat_write(), incoming.compat()),
            async |connection| {
                serve_connection(&spec, connection, &callback, &mut receive, &mut stopped)
                    .await
                    .map_err(|error| host_to_acp(&error))
            },
        );
        let peer = Peer {
            io: BufReader::new(peer),
            callback: callback.clone(),
            commands,
            shutdown,
        };
        let (result, ()) = tokio::join!(driver, test(peer));
        callback.cancel_questions();
        assert!(callback.waiting.lock().is_empty());
        result
    })
    .await
    .expect("ACP wire test timed out")
}

#[tokio::test]
async fn v1_startup_callbacks_and_turn_receipts() {
    run_peer(spec(None), async |mut peer| {
        // Commands are serviced even while initialize/session-new is pending.
        peer.command(
            1,
            NativeIntent::Prompt {
                text: "too early".into(),
            },
        )
        .await;
        peer.receipt(1, NativeDisposition::Refused).await;
        peer.initialize(false).await;
        let new_session = peer.recv().await;
        assert_eq!(new_session["method"], "session/new");
        peer.permission("startup-permission", "session").await;
        peer.question("acp-question:1").await;
        peer.answer(2, "wrong-question", json!({"option_id":"allow-once"}))
            .await;
        peer.receipt(2, NativeDisposition::Refused).await;
        peer.answer(3, "acp-question:1", json!({"option_id":"allow-once"}))
            .await;
        let answer = peer.recv().await;
        assert_eq!(answer["id"], "startup-permission");
        assert_eq!(
            answer["result"],
            json!({"outcome":{"outcome":"selected", "optionId":"allow-once"}})
        );
        peer.respond(&new_session, json!({"sessionId":"session"}))
            .await;
        wait_for(|| {
            peer.callback.sink.view().connected
                && peer.callback.sink.view().provider_session_id.is_some()
        })
        .await;

        peer.command(
            4,
            NativeIntent::Prompt {
                text: "hello".into(),
            },
        )
        .await;
        let prompt = peer.recv().await;
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(
            prompt["params"],
            json!({"sessionId":"session", "prompt":[{"type":"text", "text":"hello"}]})
        );
        peer.receipt(4, NativeDisposition::Queued).await;
        assert_eq!(peer.callback.sink.view().turn_id.as_deref(), Some("acp:4"));
        peer.command(
            5,
            NativeIntent::Prompt {
                text: "overlap".into(),
            },
        )
        .await;
        peer.receipt(5, NativeDisposition::Refused).await;
        peer.command(
            6,
            NativeIntent::Interrupt {
                turn_id: "stale".into(),
            },
        )
        .await;
        peer.receipt(6, NativeDisposition::Refused).await;
        peer.permission("cancel-permission", "session").await;
        peer.question("acp-question:2").await;
        peer.command(
            7,
            NativeIntent::Interrupt {
                turn_id: "acp:4".into(),
            },
        )
        .await;
        let messages = [peer.recv().await, peer.recv().await];
        let cancel = messages
            .iter()
            .find(|message| message["method"] == "session/cancel")
            .unwrap();
        let permission = messages
            .iter()
            .find(|message| message["id"] == "cancel-permission")
            .unwrap();
        assert_eq!(
            permission["result"],
            json!({"outcome":{"outcome":"cancelled"}})
        );
        assert_eq!(cancel["method"], "session/cancel");
        assert_eq!(cancel["params"]["sessionId"], "session");
        assert!(cancel.get("id").is_none());
        peer.receipt(7, NativeDisposition::Queued).await;
        assert!(peer.callback.sink.view().turn_id.is_some());
        peer.respond(&prompt, json!({"stopReason":"cancelled"}))
            .await;
        peer.receipt(4, NativeDisposition::Completed).await;
        assert!(peer.callback.sink.view().turn_id.is_none());
        assert!(peer.callback.sink.view().connected);
        peer.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn v1_denies_execution_and_requires_valid_explicit_permission() {
    run_peer(spec(None), async |mut peer| {
        peer.new_session().await;
        for (method, fields) in [
            ("fs/read_text_file", json!({"path":"/never-read"})),
            ("fs/write_text_file", json!({"path":"/never-written", "content":"no"})),
            ("terminal/create", json!({"command":"never-executed"})),
            ("terminal/output", json!({"terminalId":"never"})),
            ("terminal/release", json!({"terminalId":"never"})),
            ("terminal/wait_for_exit", json!({"terminalId":"never"})),
            ("terminal/kill", json!({"terminalId":"never"})),
            ("_unsupported", json!({})),
        ] {
            let mut fields = fields;
            fields["sessionId"] = json!("session");
            peer.send(json!({"jsonrpc":"2.0", "id":method, "method":method, "params":fields})).await;
            let answer = peer.recv().await;
            assert_eq!(answer["id"], method);
            assert_eq!(answer["error"]["code"], -32601);
        }
        peer.permission("permission", "session").await;
        peer.question("acp-question:1").await;
        // A question must not stall dispatch of notifications or other requests.
        peer.send(json!({"jsonrpc":"2.0", "method":"session/update", "params":{
            "sessionId":"session", "update":{"sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":"while blocked"}}
        }})).await;
        wait_for(|| peer.callback.rows.lock().current.is_some()).await;
        peer.permission("wrong-session", "other").await;
        assert_eq!(peer.recv().await["error"]["code"], -32602);
        assert_eq!(peer.callback.waiting.lock().len(), 1);
        peer.answer(1, "acp-question:1", json!({"option_id":"not-offered"})).await;
        let answer = peer.recv().await;
        assert_eq!(answer["id"], "permission");
        assert_eq!(answer["error"]["code"], -32602);
        peer.permission("again", "session").await;
        peer.question("acp-question:2").await;
        peer.answer(2, "acp-question:2", json!({"cancelled":true})).await;
        assert_eq!(peer.recv().await["result"], json!({"outcome":{"outcome":"cancelled"}}));
        peer.stop().await;
    }).await.unwrap();
}

#[tokio::test]
async fn cursor_extensions_use_v1_wire_names_and_explicit_answers() {
    run_peer(spec(None), async |mut peer| {
        peer.new_session().await;
        peer.send(json!({"jsonrpc":"2.0", "id":"ask", "method":"_cursor/ask_question", "params":{
            "toolCallId":"ask-tool", "title":"Choose", "questions":[{
                "id":"q", "prompt":"Which?", "options":[{"id":"a", "label":"A"}]
            }]
        }})).await;
        peer.question("acp-question:1").await;
        peer.send(json!({"jsonrpc":"2.0", "method":"_cursor/update_todos", "params":{"todos":[]}})).await;
        peer.answer(1, "acp-question:1", json!({"answers":[{"id":"q", "selected_options":["a"]}]})).await;
        assert_eq!(peer.recv().await["result"], json!({"outcome":{"outcome":"answered", "answers":[{"questionId":"q", "selectedOptionIds":["a"]}]}}));
        peer.send(json!({"jsonrpc":"2.0", "id":"plan", "method":"_cursor/create_plan", "params":{
            "toolCallId":"plan-tool", "plan":"Do something"
        }})).await;
        peer.question("acp-question:2").await;
        peer.answer(2, "acp-question:2", json!({"confirmed":false})).await;
        assert_eq!(peer.recv().await["result"], json!({"outcome":{"outcome":"rejected"}}));
        peer.stop().await;
    }).await.unwrap();
}

#[tokio::test]
async fn v1_load_replays_history_before_becoming_connected() {
    run_peer(spec(Some("saved")), async |mut peer| {
        peer.initialize(true).await;
        let load = peer.recv().await;
        assert_eq!(load["method"], "session/load");
        assert_eq!(load["params"], json!({"sessionId":"saved", "cwd":"/tmp", "mcpServers":[]}));
        peer.send(json!({"jsonrpc":"2.0", "method":"session/update", "params":{
            "sessionId":"saved", "update":{"sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":"history"}}
        }})).await;
        peer.respond(&load, json!({})).await;
        wait_for(|| peer.callback.sink.view().provider_session_id.as_deref() == Some("saved")).await;
        assert!(peer.callback.rows.lock().current.is_none());
        peer.stop().await;
    }).await.unwrap();
}

#[tokio::test]
async fn load_requires_advertised_capability_and_protocol_must_be_v1() {
    let result = run_peer(spec(Some("saved")), async |mut peer| {
        peer.initialize(false).await;
        // No load request may be sent. The host closes the transport on error.
        let mut line = String::new();
        assert_eq!(peer.io.read_line(&mut line).await.unwrap(), 0);
    })
    .await;
    assert!(result.unwrap_err().to_string().contains("session/load"));
    let result = run_peer(spec(None), async |mut peer| {
        let request = peer.recv().await;
        peer.respond(
            &request,
            json!({"protocolVersion":2, "agentCapabilities":{}, "authMethods":[]}),
        )
        .await;
        let mut line = String::new();
        assert_eq!(peer.io.read_line(&mut line).await.unwrap(), 0);
    })
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn child_transport_exit_and_shutdown_are_bounded() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut spec = spec(None);
        spec.command = vec!["/bin/sh".into(), "-c".into(), "exit 0".into()];
        let (_commands, receive) = mpsc::channel(1);
        let (_shutdown, stopped) = watch::channel(false);
        let result = run_local(spec, crate::session::tests::sink(), receive, stopped).await;
        assert!(result.is_err());

        let mut spec = self::spec(None);
        spec.command = vec!["/bin/sleep".into(), "30".into()];
        let (_commands, receive) = mpsc::channel(1);
        let (shutdown, stopped) = watch::channel(false);
        shutdown.send(true).unwrap();
        run_local(spec, crate::session::tests::sink(), receive, stopped)
            .await
            .unwrap();
    })
    .await
    .expect("ACP child was not killed/reaped promptly");
}

#[tokio::test]
async fn shutdown_and_eof_cancel_pending_startup_questions() {
    run_peer(spec(None), async |mut peer| {
        peer.initialize(false).await;
        let _new_session = peer.recv().await;
        peer.permission("pending", "session").await;
        peer.question("acp-question:1").await;
        peer.stop().await;
    })
    .await
    .unwrap();
    let result = run_peer(spec(None), async |mut peer| {
        peer.initialize(false).await;
        let _new_session = peer.recv().await;
        peer.permission("pending", "session").await;
        peer.question("acp-question:1").await;
        // Drop peer I/O while keeping control channels alive until EOF is read.
        peer.io.get_mut().shutdown().await.unwrap();
        let mut line = String::new();
        assert_eq!(peer.io.read_line(&mut line).await.unwrap(), 0);
    })
    .await;
    assert!(result.is_err());
}
