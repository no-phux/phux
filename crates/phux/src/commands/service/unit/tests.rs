use std::path::PathBuf;

use super::super::{
    HubEnsure, RESTART_THROTTLE_SECS, SERVICE_MANAGED_ENV, START_LIMIT_BURST,
    ensure_hub_in_wrapper, modernize_wrapper, render_wrapper_script,
};
use super::{
    ServicePlan, render_launchd_plist, render_systemd_unit, systemd_escape, systemd_quote,
    systemd_unquote, xml_escape, xml_unescape,
};

/// A plan with most optional fields populated.
pub(in crate::commands::service) fn plan() -> ServicePlan {
    ServicePlan {
        binary: PathBuf::from("/usr/local/bin/phux"),
        quic: Some("0.0.0.0:8788".to_owned()),
        listen: Some("0.0.0.0:8787".to_owned()),
        tokens: PathBuf::from("/home/u/.local/state/phux/remote-tokens"),
        cert: PathBuf::from("/home/u/.local/state/phux/remote-cert.pem"),
        key: PathBuf::from("/home/u/.local/state/phux/remote-key.pem"),
        socket: None,
        hub: false,
        socket_path: PathBuf::from("/run/user/1000/phux/phux.sock"),
        profile: None,
        log: PathBuf::from("/home/u/.local/state/phux/server.log"),
        restore: None,
        wrapper: PathBuf::from("/home/u/.local/state/phux/service-wrapper.sh"),
    }
}

#[test]
fn launchd_plist_carries_the_auth_environment() {
    let plist = render_launchd_plist(&plan());
    assert!(plist.contains("<key>PHUX_WS_TOKENS</key>"));
    assert!(plist.contains("<string>/home/u/.local/state/phux/remote-tokens</string>"));
    assert!(plist.contains("<key>PHUX_QUIC_ADDR</key>"));
    assert!(plist.contains("<string>0.0.0.0:8788</string>"));
    assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("<string>com.phux.server</string>"));
}

/// Both generated units carry [`SERVICE_MANAGED_ENV`] unconditionally.
#[test]
fn both_units_carry_the_service_managed_marker() {
    let plist = render_launchd_plist(&plan());
    assert!(plist.contains(&format!("<key>{SERVICE_MANAGED_ENV}</key>")));
    assert!(plist.contains("<string>1</string>"));

    let unit = render_systemd_unit(&plan());
    assert!(unit.contains(&format!("Environment=\"{SERVICE_MANAGED_ENV}=1\"")));
}

#[test]
fn hub_flag_reaches_direct_units_and_restore_wrapper() {
    let mut plan = plan();
    plan.hub = true;
    assert!(
        render_launchd_plist(&plan).contains("<string>server</string>\n    <string>--hub</string>")
    );
    assert!(render_systemd_unit(&plan).contains("ExecStart=/usr/local/bin/phux server --hub"));

    plan.restore = Some(PathBuf::from("/home/u/archive.json"));
    let script = render_wrapper_script(&plan);
    assert!(script.contains("\"$phux\" server --hub --autosave \"$archive\" &"));
    // The generated wrapper is already current for the `--hub` patcher.
    assert!(matches!(ensure_hub_in_wrapper(&script), HubEnsure::Current));
}

#[test]
fn launchd_runs_the_wrapper_when_restore_is_on() {
    let mut plan = plan();
    plan.restore = Some(PathBuf::from("/home/u/.local/state/phux/workspace.json"));
    let plist = render_launchd_plist(&plan);
    assert!(plist.contains("<string>/bin/sh</string>"));
    assert!(plist.contains("<string>/home/u/.local/state/phux/service-wrapper.sh</string>"));
    // The server is started by the wrapper, not by launchd.
    assert!(!plist.contains("<string>server</string>"));
}

/// Both units restart on failure only and throttle, and systemd's start
/// limit window admits the throttle so a crash-loop is eventually given up.
#[test]
fn both_units_restart_only_on_failure_and_throttle() {
    let plist = render_launchd_plist(&plan());
    assert!(
        plist.contains("<key>SuccessfulExit</key>\n    <false/>"),
        "launchd must not restart after a clean exit — `phux kill --server` \
         has to stay dead.\n{plist}"
    );
    assert!(
        !plist.contains("<key>KeepAlive</key>\n  <true/>"),
        "the unconditional KeepAlive is the defect; it must not come back.\n{plist}"
    );
    assert!(
        plist.contains("<key>ThrottleInterval</key>"),
        "an unthrottled respawn hides a crash-loop.\n{plist}"
    );

    let unit = render_systemd_unit(&plan());
    assert!(
        unit.contains("Restart=on-failure"),
        "systemd must match launchd's failure-only policy.\n{unit}"
    );
    assert!(
        !unit.contains("Restart=always"),
        "`Restart=always` is systemd's spelling of the same defect.\n{unit}"
    );
    assert!(
        unit.contains(&format!("RestartSec={RESTART_THROTTLE_SECS}s")),
        "systemd's throttle must match launchd's.\n{unit}"
    );
    assert!(
        unit.contains(&format!("StartLimitBurst={START_LIMIT_BURST}")),
        "without a start limit, a permanently-failing start retries forever.\n{unit}"
    );
    let window: u32 = RESTART_THROTTLE_SECS * (START_LIMIT_BURST + 1);
    assert!(
        unit.contains(&format!("StartLimitIntervalSec={window}s")),
        "the limit window must admit the throttle, or the burst can never be reached.\n{unit}"
    );
    assert!(
        window > RESTART_THROTTLE_SECS * START_LIMIT_BURST,
        "a window that does not fit {START_LIMIT_BURST} throttled starts makes the \
         limit unreachable, which is the bug it exists to fix"
    );
}

#[test]
fn omitted_listeners_emit_no_environment_key() {
    let mut plan = plan();
    plan.quic = None;
    plan.listen = None;
    let plist = render_launchd_plist(&plan);
    assert!(!plist.contains("PHUX_QUIC_ADDR"));
    assert!(!plist.contains("PHUX_WS_ADDR"));
    // The auth material is unconditional — it is not a listener.
    assert!(plist.contains("PHUX_WS_TOKENS"));

    let unit = render_systemd_unit(&plan);
    assert!(!unit.contains("PHUX_QUIC_ADDR"));
    assert!(unit.contains("PHUX_WS_TOKENS"));
}

#[test]
fn socket_override_reaches_both_units_and_the_wrapper() {
    let mut plan = plan();
    plan.socket = Some(PathBuf::from("/tmp/custom/phux.sock"));
    plan.socket_path = PathBuf::from("/tmp/custom/phux.sock");
    plan.restore = Some(PathBuf::from("/home/u/archive.json"));
    assert!(render_launchd_plist(&plan).contains("<key>PHUX_SOCKET</key>"));
    assert!(
        render_systemd_unit(&plan).contains("Environment=\"PHUX_SOCKET=/tmp/custom/phux.sock\"")
    );
    // The wrapper's own phux invocations target the same socket.
    let script = render_wrapper_script(&plan);
    assert_eq!(
        script.matches("--socket '/tmp/custom/phux.sock'").count(),
        2
    );
}

/// The server autosaves and restores the archive itself (ADR-0150); the
/// wrapper only adds the final save on stop.
#[test]
fn wrapper_autosaves_through_the_server_and_saves_on_term() {
    let mut plan = plan();
    plan.restore = Some(PathBuf::from("/home/u/archive.json"));
    let script = render_wrapper_script(&plan);
    assert!(script.starts_with("#!/bin/sh\n"));
    assert!(script.contains("archive='/home/u/archive.json'"));
    assert!(script.contains("\"$phux\" server --autosave \"$archive\" &"));
    assert!(script.contains("trap 'save;"), "must save on stop");
    assert!(script.contains("TERM INT"));
    assert!(script.contains("archive export --output \"$archive\""));
    assert!(!script.contains("workspace"), "the deprecated verb is gone");
}

/// A wrapper an older install wrote still calls `workspace save`, which
/// the binary no longer parses; reconcile rewrites it, and leaves a
/// current wrapper alone.
#[test]
fn reconcile_moves_an_installed_wrapper_to_archive_export() {
    let old = "save() {\n    \"$phux\" workspace save --output \"$archive\"\n}\n";
    let patched = modernize_wrapper(old).expect("an old wrapper is rewritten");
    assert!(patched.contains("\"$phux\" archive export --output \"$archive\""));
    assert!(!patched.contains("workspace save"));
    assert_eq!(modernize_wrapper(&patched), None, "idempotent");
}

#[test]
fn xml_metacharacters_in_paths_are_escaped() {
    assert_eq!(xml_escape("a&b"), "a&amp;b");
    assert_eq!(xml_escape("<x>"), "&lt;x&gt;");
    assert_eq!(xml_escape("say \"hi\""), "say &quot;hi&quot;");
    let mut plan = plan();
    plan.binary = PathBuf::from("/opt/a&b/phux");
    assert!(render_launchd_plist(&plan).contains("<string>/opt/a&amp;b/phux</string>"));
    assert!(!render_launchd_plist(&plan).contains("a&b"));
}

/// Paths with spaces are quoted, and `%`/`$` are doubled so systemd does not
/// expand them.
#[test]
fn systemd_escape_doubles_percent_and_dollar() {
    assert_eq!(systemd_escape("/usr/bin/phux"), "/usr/bin/phux");
    assert_eq!(systemd_escape("/opt/my phux/bin"), "\"/opt/my phux/bin\"");
    assert_eq!(systemd_escape("/opt/100%/bin"), "\"/opt/100%%/bin\"");
    assert_eq!(systemd_escape("/opt/$HOME/bin"), "\"/opt/$$HOME/bin\"");
    assert_eq!(systemd_quote("100%"), "100%%");
    assert_eq!(systemd_quote("$FOO"), "$$FOO");
    assert_eq!(systemd_quote("${FOO}"), "$${FOO}");
}

/// The unescapers are exact inverses of the escapers.
#[test]
fn the_unescapers_invert_the_escapers() {
    for value in [
        "/plain/path",
        "100%",
        "$FOO",
        "$$",
        "%%",
        "a&b",
        "&amp;",
        "<x>",
        "say \"hi\"",
        "back\\slash",
        "\\$mixed%",
    ] {
        assert_eq!(xml_unescape(&xml_escape(value)), value, "xml: {value}");
        assert_eq!(
            systemd_unquote(&systemd_quote(value)),
            value,
            "systemd: {value}"
        );
    }
}
