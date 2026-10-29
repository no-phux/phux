use super::*;

#[test]
fn choices_round_trip_and_junk_is_off() {
    for value in [
        "off",
        "silk",
        "aurora",
        "nebula",
        "rain",
        "grid",
        "file:mine.wgsl",
    ] {
        assert_eq!(Choice::parse(value).id(), value);
    }
    for junk in [
        "",
        "lava",
        "file:",
        "file:../escape.wgsl",
        "file:C:x.wgsl",
        "file:.hidden.wgsl",
        "file:a/b.wgsl",
        "file:notes.txt",
    ] {
        assert_eq!(Choice::parse(junk), Choice::Off, "{junk}");
    }
    assert_eq!(Choice::parse("file:mine.wgsl").label(), "mine");
    assert_eq!(Choice::parse("rain").label(), "Rain");
}

#[test]
fn builtin_ids_are_unique() {
    let mut ids: Vec<_> = BUILTINS.iter().map(|builtin| builtin.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), BUILTINS.len());
    assert!(!ids.contains(&OFF));
}

#[test]
fn the_folder_lists_shaders_and_new_starters_never_overwrite() {
    let dir = std::env::temp_dir().join(format!("phux-shaders-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(files_in(&dir).is_empty());
    let first = create_starter(&dir).expect("writes");
    let second = create_starter(&dir).expect("writes");
    assert_eq!(first, "my-backdrop.wgsl");
    assert_eq!(second, "my-backdrop-2.wgsl");
    std::fs::write(dir.join("Zed.wgsl"), "x").expect("writes");
    std::fs::write(dir.join("notes.txt"), "x").expect("writes");
    std::fs::create_dir(dir.join("dir.wgsl")).expect("mkdir");
    assert_eq!(
        files_in(&dir),
        ["my-backdrop-2.wgsl", "my-backdrop.wgsl", "Zed.wgsl"]
    );
    let loaded = load_file(&dir.join(&first), &first);
    assert!(loaded.program.is_ok(), "{:?}", loaded.program);
    assert!(loaded.modified.is_some());
    let broken = load_file(&dir.join("Zed.wgsl"), "Zed.wgsl");
    assert!(broken.program.is_err());
    let huge = dir.join("huge.wgsl");
    std::fs::write(&huge, vec![b' '; MAX_SOURCE_BYTES + 1]).expect("writes");
    let refused = load_file(&huge, "huge.wgsl").program.expect_err("too big");
    assert!(refused.message.contains("limit"), "{refused}");
    std::fs::remove_file(&huge).expect("removes");
    #[cfg(unix)]
    {
        let linked = dir.join("linked.wgsl");
        std::os::unix::fs::symlink(dir.join(&first), &linked).expect("links");
        assert!(files_in(&dir).contains(&"linked.wgsl".to_owned()));
        std::fs::remove_file(&linked).expect("unlinks");
    }
    let missing = load_file(&dir.join("gone.wgsl"), "gone.wgsl");
    assert!(
        missing
            .program
            .expect_err("missing")
            .message
            .contains("could not read")
    );
    std::fs::remove_dir_all(&dir).expect("cleanup");
}
