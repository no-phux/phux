/// Prefix `r` reopens hidden terminals and prefix `v` closes the focused
/// view. Neither is bound by default.
fn hidden_view_env(fence_up: bool) -> Env<'static> {
    let mut cfg = default_cfg();
    cfg.keybindings.prefix_table.insert(
        "r".to_owned(),
        phux_config::Action::Bare("reopen-views".to_owned()),
    );
    cfg.keybindings.prefix_table.insert(
        "v".to_owned(),
        phux_config::Action::Bare("close-view".to_owned()),
    );
    let mut env = Env::new(CtxFixture::default());
    env.resolver = Some(phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver"));
    env.keybindings = Some(cfg.keybindings);
    env.fx.layout_read_complete = fence_up;
    env.fx.focused_session = Some(phux_protocol::ids::SessionId::new(1));
    env.fx.session_members.extend([tid(1), tid(2), tid(4)]);
    env
}

fn sent_set_metadata(frames: &[FrameKind]) -> bool {
    frames
        .iter()
        .any(|frame| matches!(frame, FrameKind::SetMetadata { .. }))
}

/// #1475: `close-view` and `reopen-views` edit the arrangement. While the
/// initial layout read is outstanding that edit must not land on the
/// fallback workspace, and it must not be dropped: it is queued, applied
/// to the adopted workspace, and that result is what gets saved.
#[tokio::test(flavor = "current_thread")]
async fn hidden_view_actions_wait_for_the_layout_read_fence() {
    use tracing::instrument::WithSubscriber;

    let buf = InfoLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(buf.clone())
        .finish();
    let subscriber = tracing::Dispatch::new(subscriber);

    let mut env = hidden_view_env(false);
    let before = crate::attach::arrangement_shape(&env.fx.workspace);
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::R, Some("r"))])
        .with_subscriber(subscriber.clone())
        .await;
    assert_eq!(
        crate::attach::arrangement_shape(&env.fx.workspace),
        before,
        "reopen-views must not edit the fallback workspace before the layout read"
    );
    assert!(
        !sent_set_metadata(&sent.frames),
        "no save while the fence is down: {:?}",
        sent.frames
    );
    assert_eq!(
        env.fx.deferred_arrangement_edits,
        vec![super::deferred::DeferredArrangementEdit::ReopenViews]
    );
    let changed = super::deferred::apply_deferred_arrangement_edits(
        &mut env.fx.deferred_arrangement_edits,
        &mut env.fx.workspace,
        &env.fx.session_members,
        &env.panes,
        &mut env.fx.zoomed,
        &mut env.focused,
    );
    assert!(changed, "the queued reopen changes the adopted workspace");
    assert!(env.fx.deferred_arrangement_edits.is_empty());
    assert_eq!(env.fx.workspace.groups.len(), 3);

    let mut env = hidden_view_env(false);
    let before = crate::attach::arrangement_shape(&env.fx.workspace);
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::V, Some("v"))])
        .with_subscriber(subscriber)
        .await;
    assert_eq!(
        crate::attach::arrangement_shape(&env.fx.workspace),
        before,
        "close-view must not edit the fallback workspace before the layout read"
    );
    assert!(!sent_set_metadata(&sent.frames), "{:?}", sent.frames);
    assert_eq!(
        env.fx.deferred_arrangement_edits,
        vec![super::deferred::DeferredArrangementEdit::CloseView { target: tid(1) }]
    );
    let changed = super::deferred::apply_deferred_arrangement_edits(
        &mut env.fx.deferred_arrangement_edits,
        &mut env.fx.workspace,
        &env.fx.session_members,
        &env.panes,
        &mut env.fx.zoomed,
        &mut env.focused,
    );
    assert!(changed);
    assert!(
        env.fx.workspace.groups.is_empty(),
        "the queued close folds the adopted view"
    );

    let log = buf.text();
    assert!(
        log.contains("arrangement save deferred") && log.contains("layout read fence"),
        "{log}"
    );
    assert!(
        log.contains("reopen-views") && log.contains("close-view"),
        "{log}"
    );
}

/// In-memory tracing sink for an info-level subscriber.
#[derive(Clone, Default)]
struct InfoLog(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for InfoLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for InfoLog {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl InfoLog {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("log lock").clone()).expect("utf8 log")
    }
}

/// Releasing the bootstrap fence on failure does not authorize editing an
/// arrangement the link has never loaded; a new action owes one retry.
#[tokio::test(flavor = "current_thread")]
async fn hidden_view_actions_after_a_failed_read_queue_and_request_one_retry() {
    for (key, text) in [(PhysicalKey::R, "r"), (PhysicalKey::V, "v")] {
        let mut env = hidden_view_env(true);
        env.fx.unloaded_arrangement_read = Some(false);
        let before = env.fx.workspace.clone();
        let sent = env.dispatch(vec![ctrl_a(), press(key, Some(text))]).await;
        assert_eq!(env.fx.workspace, before);
        assert_eq!(env.fx.deferred_arrangement_edits.len(), 1);
        assert_eq!(env.fx.unloaded_arrangement_read, Some(true));
        assert!(!sent_set_metadata(&sent.frames));
    }
}

/// Once the layout read has landed, the same reopen saves the extra windows.
#[tokio::test]
async fn reopen_views_after_the_layout_read_saves() {
    let mut env = hidden_view_env(true);
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::R, Some("r"))])
        .await;
    assert_eq!(env.fx.workspace.groups.len(), 3);
    assert!(
        sent_set_metadata(&sent.frames),
        "the save is the arrangement a reattach will see: {:?}",
        sent.frames
    );
}
