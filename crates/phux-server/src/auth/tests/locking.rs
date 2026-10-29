use super::*;

#[test]
fn hostile_precreated_lock_and_untrusted_parent_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let lock_path = credential_lock_path(&path);
    let target = dir.path().join("attacker-lock-target");
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&target)
        .unwrap();
    std::os::unix::fs::symlink(&target, &lock_path).unwrap();
    assert!(matches!(
        mint_token(&path),
        Err(AuthError::InsecureStore(_))
    ));

    fs::remove_file(&lock_path).unwrap();
    fs::write(&lock_path, b"hostile").unwrap();
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        mint_token(&path),
        Err(AuthError::InsecureStore(_))
    ));

    fs::remove_file(&lock_path).unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o770)).unwrap();
    assert!(matches!(
        mint_token(&path),
        Err(AuthError::InsecureStore(_))
    ));
}

#[test]
fn replacing_lock_path_cannot_split_concurrent_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    mint_token(&path).unwrap();

    let acquired = std::sync::Arc::new(std::sync::Barrier::new(2));
    let release = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first_path = path.clone();
    let first_acquired = acquired.clone();
    let first_release = release.clone();
    let first = std::thread::spawn(move || {
        with_store_lock(&first_path, || {
            first_acquired.wait();
            first_release.wait();
            mint_credential_unlocked(
                &first_path,
                None,
                &[TERMINAL_CONTROL_SCOPE.to_owned()],
                None,
            )
        })
    });
    acquired.wait();

    let lock_path = credential_lock_path(&path);
    fs::rename(&lock_path, dir.path().join("displaced-lock")).unwrap();
    OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .unwrap();

    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    let second_path = path.clone();
    let second = std::thread::spawn(move || {
        let result = mint_token(&second_path);
        completed_tx.send(()).unwrap();
        result
    });
    assert!(
        completed_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "replacement lock inode must not admit a concurrent writer"
    );

    release.wait();
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    assert_eq!(
        TokenStore::load(&path).unwrap().len(),
        3,
        "both serialized mutations survive lock-path replacement"
    );
}

#[test]
fn credential_lock_child() {
    let Some(path) = std::env::var_os("PHUX_TEST_CREDENTIAL_STORE") else {
        return;
    };
    mint_credential(
        Path::new(&path),
        None,
        &[TERMINAL_CONTROL_SCOPE.to_owned()],
        None,
    )
    .unwrap();
}

#[test]
fn concurrent_process_writers_do_not_lose_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let executable = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for _ in 0..8 {
        children.push(
            std::process::Command::new(&executable)
                .args(["--exact", "auth::tests::locking::credential_lock_child"])
                .env("PHUX_TEST_CREDENTIAL_STORE", &path)
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(TokenStore::load(&path).unwrap().len(), 8);
}

#[test]
fn concurrent_rotation_cannot_resurrect_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let first = mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
    let original = secret(&first);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let rotate_path = path.clone();
    let rotate_id = first.id.clone();
    let rotate_barrier = barrier.clone();
    let rotate = std::thread::spawn(move || {
        rotate_barrier.wait();
        rotate_credential(&rotate_path, &rotate_id, Duration::minutes(5))
    });
    let revoke_path = path.clone();
    let revoke_id = first.id;
    let revoke_barrier = barrier.clone();
    let revoke = std::thread::spawn(move || {
        revoke_barrier.wait();
        revoke_credential(&revoke_path, &revoke_id)
    });
    barrier.wait();
    let rotated = rotate.join().unwrap();
    revoke.join().unwrap().unwrap();
    let store = TokenStore::load(&path).unwrap();
    assert!(!store.verify(&original));
    if let Ok(rotated) = rotated {
        assert!(!store.verify(&secret(&rotated)));
    }
}
