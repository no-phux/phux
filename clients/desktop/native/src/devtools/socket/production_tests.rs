use super::*;

#[test]
fn direct_dev_requests_refuse_a_production_associated_server_before_dispatch() {
    let cli = super::super::ServerCli {
        phux: PathBuf::from("/nonexistent/installed-phux"),
        socket: PathBuf::from("/run/user/4294967294/phux/phux.sock"),
    };
    let before = (
        phux_perf::trace::armed(),
        phux_perf::alloc::enabled(),
        super::super::hud_visible(),
    );
    for op in ["perf", "server", "frames", "reset", "trace", "hud", "alloc"] {
        let reply = handle_with_cli(&json!({"op": op, "on": true, "seconds": -1}), Some(&cli));
        assert_eq!(reply["ok"], false, "{op}: {reply}");
        assert!(
            reply["error"]
                .as_str()
                .unwrap_or_default()
                .contains("production phux socket"),
            "{op}: {reply}"
        );
    }
    assert_eq!(
        (
            phux_perf::trace::armed(),
            phux_perf::alloc::enabled(),
            super::super::hud_visible()
        ),
        before
    );
}

#[test]
fn dev_socket_bind_refuses_a_production_socket_before_probing_or_unlinking() {
    let error = serve(Path::new("/run/user/4294967294/phux/phux.sock"))
        .expect_err("a production socket cannot be used for the dev socket");
    assert!(
        error.to_string().contains("production phux socket"),
        "{error:#}"
    );
}
