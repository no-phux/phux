//! Exercise the exact printed remedy through the legacy native argument parser.
use super::*;
use base64::Engine as _;

fn wsl_script(script: &str) -> String {
    let mut command = Command::new("wsl.exe");
    let encoded = base64::engine::general_purpose::STANDARD.encode(script);
    let wrapper = format!("IFS=;set -f;eval $(printf %s {encoded}|base64 -d)");
    command.args([
        "--distribution",
        &std::env::var("PHUX_WSL_DISTRO").unwrap(),
        "--exec",
        "sh",
        "-c",
        &wrapper,
    ]);
    run_bounded(
        &mut command,
        None,
        Duration::from_secs(30),
        "WSL fixture timed out",
    )
    .unwrap()
}

#[test]
#[ignore = "requires the disposable native Windows CI WSL user"]
fn powershell_51_manual_runtime_command_preserves_real_wsl_argv() {
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(wsl_script("id -un").trim(), "phux-ci");
    let root = format!("/tmp/phux-update-policy-{}", std::process::id());
    let executable = format!("{root}/person's CLI λ $(touch pwned)");
    let quoted = format!("'{}'", executable.replace('\'', "'\\''"));
    let setup = format!(
        "set -eu\nmkdir -m 700 {root}\ncat > {quoted} <<'FIXTURE'\n#!/bin/sh\nexec python3 -c 'import json,sys;print(json.dumps(sys.argv[1:]))' \"$@\"\nFIXTURE\nchmod 700 {quoted}\ncd {root}\n"
    );
    wsl_script(&setup);
    let socket = format!("{root}/person's socket λ $(touch pwned)");
    let command = runtime::manual_runtime_update(
        crate::platform::phux_command(Path::new(&executable)),
        Path::new(&socket),
        Channel::Next,
    );
    let remedy = runtime::powershell_command(&command);
    let script = format!(
        "$ErrorActionPreference = 'Stop'; {remedy}; if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}"
    );
    let utf16 = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let output = run_bounded(
        Command::new("powershell.exe").args([
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ]),
        None,
        Duration::from_secs(30),
        "Legacy PowerShell argv qualification timed out",
    );
    // Remove only the fixture created by this test; never a user's installation.
    let cleanup = wsl_script(&format!(
        "set -eu\ntest ! -e {root}/pwned\ntest ! -e pwned\nrm -- {quoted}\nrmdir -- {root}\n"
    ));
    assert!(cleanup.is_empty());
    let arguments: Vec<String> = serde_json::from_str(&output.unwrap()).unwrap();
    assert_eq!(
        arguments,
        ["--socket", socket.as_str(), "update", "--channel", "next"]
    );
}
