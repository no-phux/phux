use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::runtime::test_support::owner_peer;

const LONG: Duration = Duration::from_secs(600);
const LINK: &str = "https://id.example/auth?code=secret";
const PANE: ClientId = ClientId(9);

struct World {
    state: SharedState,
    terminal: WireResourceId,
}

impl World {
    fn new() -> Self {
        let state = SharedState::new();
        let (handle, _) = crate::runtime::apps::terminal::test_support::stub_terminal();
        let terminal = state.with_mut(|s| {
            let (_, pane) = s.seed_session("main");
            s.register_resource_handle(pane, handle, CancellationToken::new())
                .unwrap()
        });
        Self { state, terminal }
    }

    /// Client `id` attached to the pane, prompting for links when `prompts`.
    fn attach(&self, id: u64, prompts: bool) -> (CommandContext, mpsc::Receiver<Outbound>) {
        let (tx, rx) = mpsc::channel(4);
        self.state.with_mut(|s| {
            s.clients_mut()
                .set_peer_identity(ClientId(id), owner_peer());
            s.attach(
                ClientId(id),
                "main",
                tx.clone(),
                ClientCapabilities::new().with_open_url(prompts),
                BootstrapProfile::SynthesizedVtRaw,
                BootstrapLimits::default(),
            )
            .unwrap();
        });
        (CommandContext::for_test(&self.state, ClientId(id), &tx), rx)
    }

    fn types(&self, id: u64) {
        self.state
            .with_mut(|s| s.note_controller(&self.terminal, ClientId(id)));
    }

    async fn ask(&self, url: &str, deadline: Duration) -> CommandResult {
        let (tx, _rx) = mpsc::channel(1);
        let ctx = CommandContext::for_test(&self.state, PANE, &tx);
        request(&ctx, self.terminal.clone(), url, deadline).await
    }

    fn answer(&self, ctx: &CommandContext, id: u64, opened: bool) -> CommandResult {
        reply(ctx, &self.terminal, id, opened)
    }

    fn pending(&self) -> usize {
        self.state.with_mut(|s| s.open_urls_mut().pending.len())
    }
}

fn code(result: CommandResult) -> ErrorCode {
    match result {
        CommandResult::Error { code, .. } => code,
        other => panic!("expected an error, got {other:?}"),
    }
}

async fn asked(mailbox: &mut mpsc::Receiver<Outbound>) -> (u64, String) {
    match mailbox.recv().await {
        Some(Outbound::Frame(FrameKind::Event {
            terminal: Some(_),
            event: AgentEvent::OpenUrl { id, url },
            stamp: None,
        })) => (id, url),
        other => panic!("expected an open_url event, got {other:?}"),
    }
}

#[tokio::test]
async fn only_the_client_typing_in_the_pane_is_asked_and_only_it_can_answer() {
    let world = World::new();
    let (typist, mut typist_rx) = world.attach(1, true);
    let (other, mut other_rx) = world.attach(2, true);
    world.types(2);
    world.types(1);
    let answered = async {
        let (id, url) = asked(&mut typist_rx).await;
        assert_eq!(url, LINK);
        assert_eq!(code(world.answer(&other, id, true)), ErrorCode::NotFound);
        assert_eq!(world.answer(&typist, id, true), CommandResult::Ok);
        assert_eq!(code(world.answer(&typist, id, true)), ErrorCode::NotFound);
    };
    let (result, ()) = tokio::join!(world.ask(LINK, LONG), answered);
    assert_eq!(result, CommandResult::Ok);
    assert!(other_rx.try_recv().is_err(), "another viewer was asked");

    let declined = async {
        let (id, _) = asked(&mut typist_rx).await;
        assert_eq!(world.answer(&typist, id, false), CommandResult::Ok);
    };
    let (result, ()) = tokio::join!(world.ask(LINK, LONG), declined);
    assert_eq!(code(result), ErrorCode::Unavailable);
    assert_eq!(world.pending(), 0);
}

#[tokio::test]
async fn nobody_to_ask_is_refused_at_once() {
    let world = World::new();
    let nobody = || world.ask(LINK, LONG);
    assert_eq!(code(nobody().await), ErrorCode::ClientNotFound);
    // Attached and prompting, but not typing here.
    let (_ctx, mut idle) = world.attach(1, true);
    assert_eq!(code(nobody().await), ErrorCode::ClientNotFound);
    // Typing, but a client that does not prompt.
    let (_ctx, mut old) = world.attach(2, false);
    world.types(2);
    assert_eq!(code(nobody().await), ErrorCode::ClientNotFound);
    // Typing from outside: `phux send-keys` attaches nothing.
    world.state.with_mut(|s| {
        s.clients_mut().set_peer_identity(ClientId(3), owner_peer());
        s.note_controller(&world.terminal, ClientId(3));
    });
    assert_eq!(code(nobody().await), ErrorCode::ClientNotFound);
    // A watcher.
    world.types(1);
    world.state.with_mut(|s| {
        s.clients_mut()
            .mark_viewer(ClientId(1), &world.terminal, true);
    });
    assert_eq!(code(nobody().await), ErrorCode::ClientNotFound);
    assert!(idle.try_recv().is_err() && old.try_recv().is_err());
}

#[tokio::test]
async fn a_link_that_is_not_a_plain_web_link_never_reaches_a_client() {
    let world = World::new();
    let (_ctx, mut mailbox) = world.attach(1, true);
    world.types(1);
    for bad in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://bank.example@evil.example/",
        "https://a.example/\u{1b}]0;x\u{7}",
        &format!("https://a.example/{}", "x".repeat(9000)),
    ] {
        assert_eq!(code(world.ask(bad, LONG).await), ErrorCode::InvalidCommand);
    }
    assert!(mailbox.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn no_answer_a_disconnect_and_a_second_request_are_refused() {
    let world = World::new();
    let (typist, mut mailbox) = world.attach(1, true);
    world.types(1);
    assert_eq!(
        code(world.ask(LINK, DEADLINE).await),
        ErrorCode::Unavailable
    );
    let (late, _) = asked(&mut mailbox).await;
    assert_eq!(code(world.answer(&typist, late, true)), ErrorCode::NotFound);

    let started = tokio::time::Instant::now();
    let gone = async {
        asked(&mut mailbox).await;
        let second = world.ask(LINK, LONG).await;
        assert_eq!(code(second), ErrorCode::ResourceExhausted);
        world.state.with_mut(|s| s.detach(ClientId(1)));
    };
    let (result, ()) = tokio::join!(world.ask(LINK, LONG), gone);
    assert_eq!(code(result), ErrorCode::Unavailable);
    assert_eq!(started.elapsed(), Duration::ZERO, "waited for the deadline");

    // The asker's own disconnect drops its wait.
    let (_ctx, mut mailbox) = world.attach(1, true);
    world.types(1);
    tokio::select! {
        result = world.ask(LINK, LONG) => panic!("answered: {result:?}"),
        _ = asked(&mut mailbox) => {}
    }
    assert_eq!(world.pending(), 0);
}

#[tokio::test]
async fn the_audit_log_holds_the_request_and_its_outcome_without_the_query() {
    let dir = tempfile::tempdir().unwrap();
    crate::audit::capture_for_test(dir.path(), crate::audit::ROTATE_BYTES);
    let world = World::new();
    assert_eq!(code(world.ask(LINK, LONG).await), ErrorCode::ClientNotFound);
    let (typist, mut mailbox) = world.attach(1, true);
    world.types(1);
    let answered = async {
        let (id, _) = asked(&mut mailbox).await;
        world.answer(&typist, id, true);
    };
    let (_, ()) = tokio::join!(world.ask(LINK, LONG), answered);
    let log = std::fs::read_to_string(dir.path().join(crate::audit::LOG_FILE)).unwrap();
    crate::audit::release_test_capture();
    assert!(!log.contains("secret"), "{log}");
    let outcomes: Vec<_> = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|record| record["kind"] == "open_url")
        .map(|record| {
            assert_eq!(record["detail"]["host"], "id.example", "{record}");
            (
                record["outcome"].as_str().unwrap().to_owned(),
                record["detail"]["client"].as_u64().unwrap(),
            )
        })
        .collect();
    let expected = [("denied", 9), ("allowed", 9), ("approved", 1)];
    assert_eq!(
        outcomes,
        expected.map(|(outcome, client)| (outcome.to_owned(), client))
    );
}
