//! Real stdio MCP and a real isolated server, including a held workload grant.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
use phux_protocol::policy::PeerIdentity;
use phux_protocol::scope::TerminalScopeSet;
use phux_server::auth::AuthenticatedCredential;
use phux_server::policy::{ConnectionGrant, GrantFuture, PolicyEngine};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
const DEADLINE: Duration = Duration::from_secs(20);
const KEY: &str = "0123456789abcdef0123456789abcdef";
#[derive(Debug, Default)]
struct HeldSignal(AtomicBool);
impl PolicyEngine for HeldSignal {
    fn authorize_hello<'a>(
        &'a self,
        _: &'a PeerIdentity,
        _: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let held = self.0.load(Ordering::SeqCst);
        Box::pin(async move {
            Ok(if held {
                ConnectionGrant::scoped(
                    TerminalScopeSet::parse_all(&[
                        "inventory,observe,bind,?signal@global".to_owned()
                    ])
                    .unwrap(),
                    None,
                )
                .unwrap()
            } else {
                ConnectionGrant::owner()
            })
        })
    }
}
struct Mcp {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    id: u32,
    catalog: Value,
}
impl Mcp {
    async fn start(socket: &std::path::Path) -> Self {
        let program = ["CARGO_BIN_EXE_phux-mcp", "NEXTEST_BIN_EXE_phux-mcp"]
            .into_iter()
            .find_map(std::env::var_os)
            .unwrap();
        let mut command = Command::new(program);
        for key in [
            "PHUX_WS_TOKENS",
            "PHUX_WS_TLS_CERT",
            "PHUX_WS_TLS_KEY",
            "PHUX_SERVICE_MANAGED",
        ] {
            command.env_remove(key);
        }
        let mut child = command
            .env("PHUX_SOCKET", socket)
            .env("PHUX_PROFILE", "mcp-ergonomics-test")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Self {
            child,
            input,
            output,
            id: 0,
            catalog: Value::Null,
        };
        let initialized = mcp
            .request("initialize", json!({"protocolVersion":"2025-06-18"}))
            .await;
        assert_eq!(initialized["protocolVersion"], "2025-06-18");
        mcp.catalog = mcp.request("tools/list", json!({})).await["tools"].clone();
        mcp
    }
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let message = json!({"jsonrpc":"2.0", "id":self.id, "method":method, "params":params});
        self.input
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
        self.input.flush().await.unwrap();
        let mut line = String::new();
        tokio::time::timeout(DEADLINE, self.output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], self.id);
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
    async fn call(&mut self, name: &str, args: Value) -> Value {
        let result = self
            .request("tools/call", json!({"name":name,"arguments":args}))
            .await;
        let document = &result["structuredContent"];
        assert!(document.is_object(), "{result}");
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            *document
        );
        if result["isError"] == false {
            let tool = self
                .catalog
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap();
            let validator = jsonschema::validator_for(&tool["outputSchema"]).unwrap();
            assert!(validator.is_valid(document), "{name}: {document}");
        }
        result
    }
}
#[test]
fn stdio_mutation_returns_without_human_wait_and_only_authorized_approval_releases_it() {
    phux_server_testkit::run_local(async {
        tokio::time::timeout(Duration::from_secs(60), async {
            let temp = tempfile::tempdir().unwrap();
            let socket = temp.path().join("mcp.sock");
            let policy = Arc::new(HeldSignal::default());
            let engine: Arc<dyn PolicyEngine> = policy.clone();
            let (shutdown, server) = phux_server_testkit::spawn_server_with(socket.clone(), Some("main"), move |cfg| cfg.policy_engine = Some(engine));
            drop(phux_server_testkit::wait_for_socket(&socket, DEADLINE).await);
            let mut owner = phux_client::attach::connection::Connection::connect(&socket).await.unwrap();
            let mut mcp = Mcp::start(&socket).await;
            let spawned = mcp.call("phux_spawn", json!({"command":["sh", "-c", "printf MCP_READY; exec sleep 3600"]})).await;
            assert_eq!(spawned["isError"], false, "{spawned}");
            let resource = spawned["structuredContent"]["resource_id"].as_str().unwrap().to_owned();
            let ready = mcp.call("phux_wait", json!({"resource_id":resource,"until":"MCP_READY","timeout_secs":20})).await;
            assert_eq!(ready["structuredContent"]["outcome"], "met", "wait for the actual child, not just its spawn receipt: {ready}");
            let screen = mcp.call("phux_snapshot", json!({"resource_id":resource})).await;
            assert_eq!(screen["isError"], false, "{screen}");
            let confirmation = mcp.call("phux_signal", json!({"resource_id":resource,"signal":"kill"})).await;
            assert_eq!(confirmation["isError"], true);
            assert_eq!(confirmation["structuredContent"]["error"]["code"], "confirmation_required");
            policy.0.store(true, Ordering::SeqCst);
            let args = json!({"resource_id":resource,"signal":"kill","confirm":true,"idempotency_key":KEY,"background":true});
            let pending = mcp.call("phux_signal", args.clone()).await;
            assert_eq!(pending["structuredContent"]["state"], "pending");
            let replay = mcp.call("phux_signal", args.clone()).await;
            assert_eq!(replay["structuredContent"]["operation_id"], KEY);
            let approvals = tokio::time::timeout(DEADLINE, async {
                loop {
                    let approvals = phux_client::approvals::list(&mut owner).await.unwrap();
                    if !approvals.is_empty() { break approvals; }
                    let status = mcp.call("phux_operation_status", json!({"idempotency_key":KEY})).await;
                    assert_eq!(status["structuredContent"]["state"], "pending", "mutation before approval: {status}");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            assert_eq!(approvals.len(), 1, "one invocation and one approval despite a retry");
            let id = approvals[0].id.as_str();
            let listed = mcp.call("phux_approvals", json!({})).await;
            assert_eq!(listed["isError"], false, "{listed}");
            assert_eq!(listed["structuredContent"]["approvals"][0]["id"], id);
            let refused = mcp.call("phux_approve", json!({"id":id,"decision":"approve","confirm":true})).await;
            assert_eq!(refused["isError"], true, "held workloads must not approve themselves");
            assert_eq!(refused["structuredContent"]["error"]["code"], "approval_refused");
            let status = mcp.call("phux_operation_status", json!({"idempotency_key":KEY})).await;
            assert_eq!(status["structuredContent"]["state"], "pending");
            // The policy change admits the separate approver's future connections;
            // it never changes the original requester's already-minted held grant.
            policy.0.store(false, Ordering::SeqCst);
            let mut approver = Mcp::start(&socket).await;
            let visible = approver.call("phux_approvals", json!({})).await;
            assert_eq!(visible["structuredContent"]["approvals"][0]["id"], id);
            let decision = approver.call("phux_approve", json!({"id":id,"decision":"approve","confirm":true})).await;
            assert_eq!(decision["isError"], false, "{decision}");
            let completed = tokio::time::timeout(DEADLINE, async {
                loop {
                    let status = mcp.call("phux_operation_status", json!({"idempotency_key":KEY})).await;
                    if status["structuredContent"]["state"] != "pending" { break status; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            assert_eq!(completed["structuredContent"]["state"], "completed", "{completed}");
            assert_eq!(completed["structuredContent"]["result"]["signaled"], true);
            let mut foreground = args;
            foreground["background"] = json!(false);
            let replay = mcp.call("phux_signal", foreground).await;
            assert_eq!(replay["isError"], false, "replay succeeds even after its target exited");
            approver.child.kill().await.unwrap();
            mcp.child.kill().await.unwrap();
            owner.shutdown().await;
            drop(shutdown);
            server.await.unwrap().unwrap();
        }).await.expect("bounded MCP approval scenario");
    });
}
