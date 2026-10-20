//! `CHECKOUT` on a satellite Terminal runs on the satellite, against the
//! checkout that Terminal is in, and its answer returns through the hub.

use super::*;
use phux_protocol::checkout::{CheckoutAnswer, CheckoutArea, CheckoutRequest};

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn checkout_on_a_satellite_terminal_is_answered_by_the_satellite() {
    phux_server_testkit::run_local(async {
        let repo_dir = TempDir::new().unwrap();
        let repo = repo_dir.path().canonicalize().unwrap();
        git(&repo, &["init", "-q", "-b", "trunk"]);
        std::fs::write(repo.join("satellite.txt"), "hello\n").unwrap();
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.arg("-c");
        seed.arg("sleep 60");
        seed.cwd(&repo);
        let fed = Fed::boot_with(Some(seed), None).await;
        let mut hub = fed.linked_hub().await;

        let (result, _) = command_via_hub(
            &mut hub,
            1,
            Command::Checkout {
                terminal_id: fed.sat_id(),
                request: CheckoutRequest::Status,
            },
        )
        .await;
        let CommandResult::OkWith(CommandValue::Checkout(answer)) = result else {
            panic!("CHECKOUT via the hub: {result:?}");
        };
        let CheckoutAnswer::Status(status) = *answer else {
            panic!("expected status: {answer:?}");
        };
        assert_eq!(status.root, repo.to_str().unwrap());
        assert_eq!(status.branch.as_deref(), Some("trunk"));
        assert!(
            status
                .entries
                .iter()
                .any(|entry| entry.area == CheckoutArea::Untracked && entry.path == "satellite.txt"),
            "{status:?}"
        );

        fed.shutdown().await;
    });
}
