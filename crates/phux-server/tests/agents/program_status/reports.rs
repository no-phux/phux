//! Report parsing as L3 sees it: feature detection, and which malformed
//! input is skipped versus discarded whole.

use super::*;

#[test]
fn feature_reply_precedes_device_attributes_and_does_not_create_a_record() {
    run_local(async {
        let mut pane = Pane::start(&[Vec::new()], false).await;
        let bytes = pane.release(0).await;
        let tail = bytes
            .strip_prefix(
                b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth"
                    .as_slice(),
            )
            .expect("OSC feature detection reply must arrive before DA");
        let da = tail
            .strip_prefix(b"\x1b\\")
            .or_else(|| tail.strip_prefix(b"\x07"))
            .expect("feature reply has an OSC terminator");
        assert!(
            da.starts_with(b"\x1b["),
            "DA follows feature detection: {bytes:?}"
        );
        assert!(da.ends_with(b"c"));
        assert_eq!(pane.get(RESOURCE_PROGRAM_STATUS_KEY).await, None);
        pane.assert_absent(&[""]).await;
        pane.stop().await;
    });
}

#[test]
fn invalid_reports_are_atomic_but_malformed_pairs_and_unknown_keys_are_skipped() {
    run_local(async {
        let initial =
            "id=task:state=blocked:kind=auth:progress=12:app=deploy:title=TG9naW4=:msg=VG9rZW4/";
        let mut invalid = reports(&[
            "id=task:state=working:msg=A===",
            "id=task:state=working:msg=AA==",
            "id=task:state=working:msg=/w==",
            "id=task:state=working:msg=woA=",
            "id=task:state=working:title=wqEAcQ==",
            "id=task:state=future:app=changed",
            "id=task:app=changed",
            "id=/task:state=clear",
            "id=task/:state=clear",
            "id=task//child:state=working",
            "id=a/b/c/d/e/f/g/h/i:state=working",
            // Bytes outside the value set make the id malformed, not absent:
            // skipping the pair would aim the report, and the clear, at root.
            "id=a@b:state=clear",
            "id=my task:state=clear",
            "id=task:id=t@sk:state=working:app=changed",
            "id=task:state=clear:msg=AA==",
        ]);
        invalid.extend(report(&format!(
            "id=task:state=working:title={}",
            STANDARD.encode("x".repeat(193))
        )));
        invalid.extend(report(&format!(
            "id=task:state=clear:msg={}",
            STANDARD.encode("x".repeat(2049))
        )));
        invalid.extend(report(&format!("id={}:state=working", "x".repeat(33))));
        invalid.extend(report(&format!(
            "id={}:state=working",
            vec!["x".repeat(32); 4].join("/")
        )));
        invalid.extend(report(&format!(
            "id=task:state=working:{}=value",
            "k".repeat(17)
        )));
        invalid.extend(report(&format!(
            "id=task:state=working:unknown={}",
            "x".repeat(4096)
        )));
        let valid = "id=task:state=working:broken:key=not@legal:state=blocked:kind=future:progress=101:app=bad/name:unknown=ignored:msg=SGVsbG8";
        let mut pane = Pane::start(
            &[
                fenced(reports(&["state=done:app=root", initial]), 0),
                fenced(invalid, 1),
                fenced(report(valid), 2),
                fenced(report("id=task:state=done:kind=permission:progress=50"), 3),
            ],
            false,
        )
        .await;
        pane.stage(0).await;
        let before = pane.record("task").await;
        let root = pane.record("").await;
        pane.stage(1).await;
        pane.assert_ids(&["", "task"]).await;
        assert_eq!(pane.record("task").await, before);
        assert_eq!(
            pane.record("").await,
            root,
            "invalid id must never fall back to root"
        );
        pane.stage(2).await;
        assert_eq!(
            pane.record("task").await,
            json!({
                "id": "task", "state": "blocked", "app": "root", "msg": "Hello"
            })
        );
        pane.stage(3).await;
        assert_eq!(
            pane.record("task").await,
            json!({
                "id": "task", "state": "done", "app": "root"
            })
        );
        pane.stop().await;
    });
}
