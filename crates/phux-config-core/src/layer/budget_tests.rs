use super::merged_with_budget;

#[test]
fn aggregate_budget_counts_unique_diamond_layers_and_preserves_unbounded_loading() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("config.toml");
    let input = "extends=['a.toml','b.toml']\n";
    let branch = "extends=['common.toml']\n";
    let common = "[[remote]]\nname='x'\nendpoint='ssh://x'\n";
    std::fs::write(dir.path().join("a.toml"), branch).expect("a");
    std::fs::write(dir.path().join("b.toml"), branch).expect("b");
    std::fs::write(dir.path().join("common.toml"), common).expect("common");
    let exact = input.len() + 2 * branch.len() + common.len();
    let merge = |budget| merged_with_budget("", input, &root, budget).map(|(table, _)| table);
    let ordinary = merge(None).expect("ordinary loader");
    assert_eq!(merge(Some(exact)).expect("exact budget"), ordinary);
    assert!(merge(Some(exact - 1)).is_err());
    std::fs::write(
        dir.path().join("common.toml"),
        format!("{common}#{}", "界".repeat(1000)),
    )
    .expect("large common");
    assert!(merge(Some(exact)).is_err());
    assert_eq!(merge(None).expect("unbounded behavior unchanged"), ordinary);
}
