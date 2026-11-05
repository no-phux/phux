//! Build-graph enforcement complements source-level app/host seam checks.
use std::process::Command;

#[test]
fn host_and_contract_normal_build_closures_exclude_native_terminal_dependencies() {
    for package in ["phux-server", "phux-terminal-api", "phux-config"] {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("Cargo manifest directory");
        let output = Command::new(env!("CARGO"))
            .args([
                "tree",
                "--offline",
                "--locked",
                "-p",
                package,
                "--edges",
                "normal,build",
                "--prefix",
                "none",
            ])
            .current_dir(manifest)
            .output()
            .expect("cargo tree");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tree = String::from_utf8(output.stdout).expect("UTF-8 dependency tree");
        for line in tree.lines() {
            let name = line.split_whitespace().next().unwrap_or("");
            assert!(
                ![
                    "phux-terminal",
                    "libghostty-vt",
                    "libghostty-vt-sys",
                    "portable-pty",
                    "portable-pty-adopt"
                ]
                .contains(&name),
                "{package} reaches {line}"
            );
        }
    }
}
