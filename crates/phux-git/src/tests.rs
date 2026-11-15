use crate::Area;
use crate::status::{Branch, Entry, parse_status};

fn entry(area: Area, path: &str, code: char) -> Entry {
    Entry {
        area,
        path: path.into(),
        from: None,
        code,
    }
}

#[test]
fn porcelain_v2_reads_branch_and_every_kind_of_change() {
    let out = [
        "# branch.oid 4fc60502deadbeefcafe",
        "# branch.head main",
        "# branch.upstream origin/main",
        "# branch.ab +2 -1",
        "1 .M N... 100644 100644 100644 aaa bbb src/app.rs",
        "1 MM N... 100644 100644 100644 aaa bbb docs/with space.md",
        "2 R. N... 100644 100644 100644 aaa bbb R100 src/new.rs",
        "src/old.rs",
        "u UU N... 100644 100644 100644 100644 a b c conflict.txt",
        "? notes/todo.txt",
        "! ignored.log",
        "",
    ]
    .join("\0");
    let status = parse_status(&out);
    assert_eq!(
        status.branch,
        Branch {
            name: Some("main".into()),
            upstream: Some("origin/main".into()),
            ahead: 2,
            behind: 1,
        }
    );
    let mut renamed = entry(Area::Staged, "src/new.rs", 'R');
    renamed.from = Some("src/old.rs".into());
    assert_eq!(
        status.entries,
        vec![
            entry(Area::Conflicted, "conflict.txt", 'U'),
            entry(Area::Staged, "docs/with space.md", 'M'),
            renamed,
            entry(Area::Unstaged, "docs/with space.md", 'M'),
            entry(Area::Unstaged, "src/app.rs", 'M'),
            entry(Area::Untracked, "notes/todo.txt", '?'),
        ]
    );
}

#[test]
fn a_detached_head_has_no_name() {
    let status = parse_status("# branch.oid (initial)\0# branch.head (detached)\0");
    assert_eq!(status.branch.name, None);
    assert!(status.entries.is_empty());
}

#[test]
fn a_rename_cut_off_before_its_source_is_dropped() {
    let out = [
        "# branch.head main",
        "1 .M N... 100644 100644 100644 aaa bbb kept.rs",
        "2 R. N... 100644 100644 100644 aaa bbb R100 new.rs",
        "",
    ]
    .join("\0");
    let status = parse_status(&out);
    let paths: Vec<_> = status.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["kept.rs"]);
}
