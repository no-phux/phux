/// The far host of a `phux host add` enrollment: its own state, config, and
/// socket, reached through a fake `ssh` that runs the real binary there.
struct FarHost {
    root: PathBuf,
}

impl FarHost {
    fn new(dir: &Path) -> Self {
        let root = dir.join("far");
        prepare_dirs(&root);
        // `mode = "paired"`: every TLS connection must present a workload
        // certificate the registry admits (workload-auth §8).
        std::fs::write(
            root.join("config/phux/config.toml"),
            "[policy]\nmode = \"paired\"\n",
        )
        .expect("far config");
        Self { root }
    }

    fn env(&self) -> Vec<(&'static str, PathBuf)> {
        let mut env = hermetic_env(&self.root);
        env.push(("HOME", self.root.clone()));
        env.push(("PHUX_SOCKET", self.root.join("s.sock")));
        env
    }

    /// `phux ARGS` on the far host.
    fn phux(&self, args: &[&str]) -> Output {
        common::phux_cmd(crate::runner::phux_bin())
            .envs(self.env())
            .current_dir(&self.root)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run far phux")
    }

    /// The far host's server: paired policy, one QUIC listener, returned once
    /// it listens. Its log goes to `server.stderr` under the far root, for
    /// [`Self::server_log`].
    fn serve(&self, quic: &str) -> Server {
        let stderr = std::fs::File::create(self.root.join("server.stderr")).expect("stderr file");
        let child = common::phux_cmd(crate::runner::phux_bin())
            .envs(self.env())
            .env("RUST_LOG", "info")
            .current_dir(&self.root)
            .args(["server", "--quic", quic, "--exit-after-idle", "120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("spawn far server");
        let server = Server(child);
        let start = Instant::now();
        while !self.server_log().contains("QUIC listening") {
            assert!(
                start.elapsed() < READY_DEADLINE,
                "the far server never listened: {}",
                self.server_log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        server
    }

    /// A fake `ssh` that logs its argv to `ssh-calls` and its stdin to
    /// `ssh-stdin`, and runs `phux` on this far host with that stdin:
    /// `ssh -G -- HOST` names loopback, and `ssh -o BatchMode=yes HOST phux
    /// ARGS...` runs the real binary. While a `tamper-add-key` file exists
    /// beside it, `workload add-key` still enrolls on the far host but its
    /// reply arrives with the leaf relabelled as a private key, which the
    /// client must refuse after the far host committed.
    fn fake_ssh(&self, dir: &Path) -> PathBuf {
        use std::fmt::Write as _;
        let exports = self
            .env()
            .iter()
            .fold(String::new(), |mut exports, (key, value)| {
                let _ = writeln!(exports, "export {key}='{}'", value.display());
                exports
            });
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{calls}'\n\
             if [ \"$1\" = \"-G\" ]; then echo 'hostname 127.0.0.1'; exit 0; fi\n\
             shift 4\n\
             {exports}\
             case \"$*\" in *'workload add-key'*) if [ -f '{tamper}' ]; then \
               tee -a '{stdin}' | '{PHUX}' \"$@\" | sed 's/-----BEGIN CERTIFICATE-----/-----BEGIN PRIVATE KEY-----/'; exit 0; fi ;; esac\n\
             tee -a '{stdin}' | '{PHUX}' \"$@\"\n",
            calls = dir.join("ssh-calls").display(),
            stdin = dir.join("ssh-stdin").display(),
            tamper = dir.join("tamper-add-key").display(),
            PHUX = crate::runner::phux_bin(),
        );
        let path = dir.join("fake-ssh");
        std::fs::write(&path, script).expect("write fake ssh");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// What the far server logged so far, for a failure message.
    fn server_log(&self) -> String {
        std::fs::read_to_string(self.root.join("server.stderr")).unwrap_or_default()
    }

    fn credentials(&self) -> Vec<serde_json::Value> {
        json_doc(&self.phux(&["workload", "list", "--json"]))["credentials"]
            .as_array()
            .expect("credentials")
            .clone()
    }
}

/// `phux ARGS` on the enrolling client: its own sandbox, `$PHUX_SSH` at the
/// fake, and no `PHUX_WORKLOAD_*` identity in the environment.
fn client_phux(dir: &Path, ssh: &Path, args: &[&str]) -> Output {
    let client = dir.join("client");
    common::phux_cmd(crate::runner::phux_bin())
        .envs(hermetic_env(&client))
        .env("HOME", &client)
        .env("PHUX_SSH", ssh)
        .current_dir(&client)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run client phux")
}

/// The `[[remote]]` entry `host add` wrote for `name`.
fn remote_entry(dir: &Path, name: &str) -> toml::Value {
    let raw =
        std::fs::read_to_string(dir.join("client/config/phux/config.toml")).expect("client config");
    let config: toml::Value = toml::from_str(&raw).expect("client config parses");
    config["remote"]
        .as_array()
        .expect("remotes")
        .iter()
        .find(|entry| entry["name"].as_str() == Some(name))
        .expect("the remote entry")
        .clone()
}

fn entry_path(entry: &toml::Value, key: &str) -> PathBuf {
    PathBuf::from(
        entry
            .get(key)
            .and_then(toml::Value::as_str)
            .expect("a path in the remote entry"),
    )
}
