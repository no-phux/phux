//! Application fixtures: isolated preferences and real embedded clients.
//! Feature tests do not depend on renderer benchmarks or installed services.

use std::sync::Arc;

use phux_client_runtime::Client;
use rune::{Env, Runtime};

use crate::desk::{Desk, Shown};
use crate::launch::Resolved;

pub mod server;
pub mod terminal;
pub mod visual;

pub const SCALE: f32 = 2.0;
pub const SIZE: (f32, f32) = (1100.0, 720.0);

/// Shape fixtures with the same embedded fonts and family slots as `AppSpec`.
pub fn runtime<S: 'static>(
    state: S,
    view: fn(&S, &mut rune::Cx<S>) -> rune::El,
    viewport: (f32, f32),
    env: rune::Env,
) -> Runtime<S> {
    Runtime::with_text(
        state,
        view,
        viewport,
        env,
        rune::text::TextSystem::new(crate::grid::FONTS, crate::grid::FAMILIES),
    )
}

#[test]
fn runtime_shapes_with_the_app_families_and_embedded_inter() {
    let style = crate::ui::Tokens::resolve(&crate::ui::tokens::Inputs::system(false))
        .ty
        .body;
    let mut rt = runtime(
        style,
        |style, _| rune::text("Inter").font(*style),
        (320.0, 240.0),
        Env::new(false, 1.0),
    );
    assert_eq!(rt.text.families(), crate::grid::FAMILIES);
    let layout = rt.text.layout_rich("Inter", &[], style, 1.0, None);
    let line = layout.lines().next().expect("one line");
    let run = line.runs().next().expect("one Inter run");
    assert!(
        run.font().data.as_ref() == crate::grid::FONTS[0],
        "the sans slot uses the app's embedded Inter"
    );
    assert!(
        run.clusters()
            .flat_map(|cluster| cluster.glyphs())
            .all(|glyph| glyph.id != 0),
        "Inter shapes real glyphs, not missing-glyph boxes"
    );
    assert!(layout.width() > 0.0);
}

/// Exercise production preference-opening factories in a private child process.
/// `full_test_name` is the exact libtest name, without the crate prefix.
/// Call before constructing any fixture; the parent never changes its environment.
pub fn isolated_preferences(full_test_name: &str, body: impl FnOnce()) {
    const CHILD: &str = "RUNE_TEST_PRIVATE_PREFERENCES";
    const DONE: &str = "private preferences body completed:";
    if std::env::var_os(CHILD).is_some_and(|name| name == full_test_name) {
        body();
        println!("\n{DONE} {full_test_name}");
        return;
    }
    let directory = tempfile::TempDir::new().expect("private preference directory");
    let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", full_test_name, "--nocapture", "--test-threads=1"])
        .env("RUNE_PREFS_DIR", directory.path())
        .env(CHILD, full_test_name)
        .output()
        .expect("the private preference child runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{full_test_name} failed with private preferences:\n{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == format!("{DONE} {full_test_name}")),
        "the child did not complete {full_test_name}:\n{stdout}"
    );
}

#[test]
#[should_panic(expected = "the child did not complete")]
fn isolated_preferences_rejects_a_missing_test() {
    isolated_preferences("test_support::no_such_preference_test", || {
        panic!("a missing test must never execute this body");
    });
}

pub fn window() -> Desk {
    Desk::with_preferences(
        &Resolved::offline("test window"),
        None,
        rune::prefs::Prefs::memory(),
    )
}

pub fn settings_window() -> Desk {
    let mut desk = window();
    desk.settings_open = true;
    desk
}

pub fn input_fixture() -> (Arc<Client>, Runtime<Desk>) {
    input_fixture_showing("")
}

pub fn input_fixture_showing(output: &str) -> (Arc<Client>, Runtime<Desk>) {
    let mut replica = terminal::Replica::new(120, 40, 1);
    if !output.is_empty() {
        replica.feed(output.to_owned());
    }
    let mut desk = window();
    desk.connection.client = Some(Arc::clone(&replica.client));
    desk.connection.seed_view_sources(vec![1; 16], &replica.sources);
    desk.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(
        replica.terminal.clone(),
    ));
    desk.connection.shown = Some(Shown {
        resource: replica.terminal,
        view: Some(replica.views[0]),
    });
    let mut rt = crate::test_support::runtime(desk, crate::ui::view, SIZE, Env::new(true, SCALE));
    rt.on_key = Some(Desk::key);
    (replica.client, rt)
}

/// The text of the notice `desk` shows; panics when it shows none.
pub fn notice(desk: &Desk) -> &str {
    &desk.notice.as_ref().expect("a notice is showing").text
}
