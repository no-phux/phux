use super::*;
use gpui::AppContext as _;

impl Desk {
    /// Show `status` with these patches, as a read of a checkout would.
    pub(crate) fn show_checkout(
        &mut self,
        status: Status,
        changes: &str,
        staged: &str,
        cx: &mut Context<Self>,
    ) {
        let parse = |text: &str| {
            let patch = git::PatchText {
                text: text.to_owned(),
                reduced: false,
            };
            Loaded::parse(patch, Vec::new())
        };
        let comparisons = Comparisons {
            changes: parse(changes),
            staged: parse(staged),
        };
        self.set_changes_view(View::Repo(status), comparisons, cx);
    }
}

#[test]
fn closing_preview_fences_reads_and_preserves_review_state() {
    let mut changes = Changes {
        generation: 42,
        reading: true,
        ..Changes::default()
    };
    let root = PathBuf::from("/review");
    changes.bind_review(&root);
    changes.focus = Some("f.rs".into());
    changes.viewed.insert((Source::Changes, "f.rs".into()), 7);
    changes.collapsed.insert("changes:src".into());
    changes
        .comparisons
        .changes
        .sections
        .insert("f.rs".into(), "patch".repeat(1024));
    changes.release_preview();
    assert_eq!(
        changes.generation, 43,
        "an outstanding generation 42 read is stale"
    );
    assert!(!changes.reading);
    assert!(changes.comparisons.changes.sections.is_empty());
    assert_eq!(changes.review_root, Some(root));
    assert_eq!(changes.focus.as_deref(), Some("f.rs"));
    assert_eq!(changes.viewed[&(Source::Changes, "f.rs".into())], 7);
    assert!(changes.collapsed.contains("changes:src"));
    assert!(changes.lapse_viewed().is_empty());
    assert_eq!(changes.viewed[&(Source::Changes, "f.rs".into())], 7);
}

#[gpui::test]
fn conflict_choices_survive_close_only_while_the_file_is_unchanged(cx: &mut gpui::TestAppContext) {
    use gpui_kit::component::diff::{DiffConflictResolution, DiffEvent};
    cx.update(gpui_kit::init);
    let text = "<<<<<<< ours\nmine\n=======\ntheirs\n>>>>>>> theirs\n";
    let loaded = || {
        Loaded::parse(
            git::PatchText::default(),
            vec![("f.rs".into(), text.into())],
        )
    };
    let mut changes = Changes::default();
    changes.comparisons.changes = loaded();
    changes.view = View::Repo(Status {
        root: PathBuf::from("/review"),
        branch: git::Branch::default(),
        entries: Vec::new(),
    });
    let files = changes.comparisons.changes.files.clone();
    let state = cx.new(|cx| DiffState::new(files, cx));
    state.update(cx, |state, cx| {
        state.resolve_conflict("f.rs", 0, Some(DiffConflictResolution::Current), cx)
    });
    let subscription = cx.update(|cx| cx.subscribe(&state, |_, _: &DiffEvent, _| {}));
    changes.diff = Some((state.clone(), subscription));
    cx.update(|cx| changes.suspend_conflicts(cx));
    changes.release_preview();
    assert_eq!(changes.suspended_conflicts.len(), 1);
    // Reopening from a different subdirectory first displays Loading.
    let loading = cx.new(|cx| DiffState::new(Vec::new(), cx));
    cx.update(|cx| changes.restore_conflicts(&loading, cx));
    assert_eq!(changes.suspended_conflicts.len(), 1);
    changes.view = View::Repo(Status {
        root: PathBuf::from("/review"),
        branch: git::Branch::default(),
        entries: Vec::new(),
    });
    changes.comparisons.changes = loaded();
    let files = changes.comparisons.changes.files.clone();
    let reopened = cx.new(|cx| DiffState::new(files, cx));
    cx.update(|cx| changes.restore_conflicts(&reopened, cx));
    assert_eq!(
        reopened
            .read_with(cx, |state, _| state.resolved_text("f.rs"))
            .as_deref(),
        Some("mine\n")
    );

    // Preserve nothing if the working conflict changed while closed.
    let subscription = cx.update(|cx| cx.subscribe(&reopened, |_, _: &DiffEvent, _| {}));
    changes.diff = Some((reopened, subscription));
    cx.update(|cx| changes.suspend_conflicts(cx));
    changes.release_preview();
    changes.comparisons.changes = Loaded::parse(
        git::PatchText::default(),
        vec![("f.rs".into(), text.replace("mine", "newer"))],
    );
    changes.view = View::Repo(Status {
        root: PathBuf::from("/review"),
        branch: git::Branch::default(),
        entries: Vec::new(),
    });
    let files = changes.comparisons.changes.files.clone();
    let changed = cx.new(|cx| DiffState::new(files, cx));
    cx.update(|cx| changes.restore_conflicts(&changed, cx));
    assert!(
        changed
            .read_with(cx, |state, _| state.conflict_resolution("f.rs", 0))
            .is_none()
    );
}

/// Observational only: RSS need not drop immediately when malloc retains pages.
#[gpui::test]
#[ignore = "isolated memory observation; run alone with --ignored --nocapture"]
fn measure_closed_preview_memory(cx: &mut gpui::TestAppContext) {
    use gpui_kit::component::diff::DiffEvent;
    use phux_perf::ProcessStats;
    cx.update(gpui_kit::init);
    let before = ProcessStats::physical_footprint_bytes();
    let heap_before = phux_perf::alloc::native::stats();
    let text = format!(
        "diff --git a/large.rs b/large.rs\n--- a/large.rs\n+++ b/large.rs\n@@ -1,50000 +1,50000 @@\n{}-old\n+new\n",
        " context line of a large source file\n".repeat(49_999)
    );
    let mut changes = Changes::default();
    changes.comparisons.changes = Loaded::parse(
        git::PatchText {
            text,
            reduced: false,
        },
        Vec::new(),
    );
    let patch_bytes: usize = changes
        .comparisons
        .changes
        .sections
        .values()
        .map(String::len)
        .sum();
    let files = changes.comparisons.changes.files.clone();
    let state = cx.new(|cx| DiffState::new(files, cx));
    let weak = state.downgrade();
    let subscription = cx.update(|cx| cx.subscribe(&state, |_, _: &DiffEvent, _| {}));
    changes.diff = Some((state, subscription));
    let loaded = ProcessStats::physical_footprint_bytes();
    let heap_loaded = phux_perf::alloc::native::stats();
    changes.release_preview();
    // Dropping an Entity queues its destruction; a production UI turn
    // flushes it before returning. Drive that turn in this isolated probe.
    cx.update(|_| {});
    cx.run_until_parked();
    assert!(weak.upgrade().is_none());
    assert!(changes.comparisons.changes.sections.is_empty());
    let closed = ProcessStats::physical_footprint_bytes();
    let heap_closed = phux_perf::alloc::native::stats();
    let returned = phux_perf::alloc::native::release_unused();
    let relieved = ProcessStats::physical_footprint_bytes();
    eprintln!(
        "preview memory bytes: baseline={before:?}, loaded={loaded:?}, closed={closed:?}, relieved={relieved:?}, returned={returned:?}, released_patch_text={patch_bytes}; malloc baseline={heap_before:?}, loaded={heap_loaded:?}, closed={heap_closed:?}"
    );
}

#[test]
fn review_notes_belong_to_one_checkout() {
    use gpui_kit::component::diff::{DiffLineRange, DiffSide};
    let mut changes = Changes::default();
    changes.bind_review(std::path::Path::new("/first"));
    changes.comments.push(Comment {
        id: 1,
        source: Source::Changes,
        range: DiffLineRange::new("f.rs", DiffSide::Modified, 1, 1),
        body: "Review only the first checkout".into(),
    });
    changes.bind_review(std::path::Path::new("/first"));
    assert_eq!(changes.comments.len(), 1);
    changes.bind_review(std::path::Path::new("/second"));
    assert!(changes.comments.is_empty());
}

#[test]
fn a_home_relative_folder_expands_and_others_stay() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    assert_eq!(expand_home("~"), home);
    assert_eq!(expand_home("~/src/phux"), home.join("src/phux"));
    assert_eq!(expand_home("/tmp/x"), PathBuf::from("/tmp/x"));
    assert_eq!(expand_home("~other/x"), PathBuf::from("~other/x"));
}

#[test]
fn auto_layout_splits_only_a_wide_diff() {
    assert_eq!(Layout::Auto.mode(SPLIT_WIDTH - 1.), DiffMode::Unified);
    assert_eq!(Layout::Auto.mode(SPLIT_WIDTH), DiffMode::Split);
    assert_eq!(Layout::Unified.mode(2000.), DiffMode::Unified);
    assert_eq!(Layout::Split.mode(10.), DiffMode::Split);
    assert_eq!(Layout::Unified.next(), Layout::Auto);
}
