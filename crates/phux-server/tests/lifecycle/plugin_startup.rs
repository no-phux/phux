//! `[[startup]]` sidecars: restart, stop, reconcile, idle. Backoff steps,
//! the cap and platform filters are unit-tested in `plugin_startup`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phux_server::plugin_startup::{
    PluginStartupConfig, PluginStartupSource, StartupCatalog, StartupSpec,
};
use phux_server::{ServerConfig, ServerError};
use phux_server_testkit::{run_local, spawn_server_with};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

fn timing_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn alive(pid: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, 0) == 0 }
}

fn spec(root: &Path, plugin: &str, command: Vec<String>, hash: u64) -> StartupSpec {
    StartupSpec {
        plugin_id: plugin.to_owned(),
        plugin_name: plugin.to_owned(),
        plugin_version: "0.1.0".to_owned(),
        startup_id: "web".to_owned(),
        command,
        plugin_root: root.to_path_buf(),
        manifest_hash: hash,
    }
}

const fn catalog(admitted: Vec<StartupSpec>) -> StartupCatalog {
    StartupCatalog {
        admitted,
        skipped_cap: Vec::new(),
    }
}

/// `parent/name`, created.
fn dir(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn sh(root: &Path, script: &str) -> Vec<String> {
    let path = root.join("run.sh");
    std::fs::write(&path, script).unwrap();
    vec!["/bin/sh".to_owned(), path.display().to_string()]
}

fn read_pid(path: &Path) -> i32 {
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// A plugin-supervising server under `tmp`.
struct Running {
    tmp: TempDir,
    state: PathBuf,
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<Result<(), ServerError>>>,
}

impl Running {
    fn boot(
        catalog: StartupCatalog,
        reload: Option<PluginStartupSource>,
        idle: Option<Duration>,
    ) -> Self {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let state = dir(tmp.path(), "state");
        let startup = PluginStartupConfig {
            catalog,
            state_dir: state.clone(),
            reload,
        };
        let (shutdown, handle) = spawn_server_with(socket, None, move |cfg: &mut ServerConfig| {
            cfg.plugin_startup = Some(Box::new(startup));
            cfg.exit_after_idle = idle;
        });
        Self {
            tmp,
            state,
            shutdown: Some(shutdown),
            handle: Some(handle),
        }
    }

    /// Boot one `plug.a` sidecar running `script` in `root`.
    fn one(root: &Path, script: &str) -> Self {
        Self::boot(
            catalog(vec![spec(root, "plug.a", sh(root, script), 1)]),
            None,
            None,
        )
    }

    async fn stop(mut self) {
        self.shutdown.take();
        let handle = self.handle.take().unwrap();
        timeout(Duration::from_secs(20), handle)
            .await
            .expect("server did not exit")
            .expect("join")
            .expect("server");
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.shutdown.take();
        let Ok(plugins) = std::fs::read_dir(self.state.join("plugin-startup")) else {
            return;
        };
        let files = plugins
            .flatten()
            .filter_map(|plugin| std::fs::read_dir(plugin.path()).ok())
            .flat_map(Iterator::flatten);
        for file in files {
            if file.path().extension().is_none_or(|ext| ext != "pid") {
                continue;
            }
            let pid = std::fs::read_to_string(file.path())
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|value| value.get("pid").and_then(serde_json::Value::as_i64));
            if let Some(pid) = pid {
                unsafe { libc::kill(i32::try_from(pid).unwrap_or(0), libc::SIGKILL) };
            }
        }
    }
}

/// Wait until generation `n` has exited and the supervisor has waited it.
///
/// `kill(pid, 0)` stays true for a zombie, so `!alive` means the supervisor
/// has already run `try_wait` and armed the next backoff. Real time is the
/// deadline: this runs while tokio time is paused.
async fn wait_reaped_generation(starts: &Path, live: &Path, n: usize) {
    let ok = wait_real(Duration::from_secs(8), || {
        read_lines(starts).len() >= n
            && std::fs::read_to_string(live)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
                .is_some_and(|pid| !alive(pid))
    })
    .await;
    assert!(
        ok,
        "generation {n} was not reaped; lines={:?} live={}",
        read_lines(starts),
        std::fs::read_to_string(live).unwrap_or_default()
    );
}

async fn wait_real(limit: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        tokio::task::yield_now().await;
    }
    pred()
}

async fn wait_until(limit: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pred()
}

async fn wait_file(path: &Path) {
    assert!(
        wait_until(Duration::from_secs(10), || path.is_file()).await,
        "{} never appeared",
        path.display()
    );
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn fresh_pid(path: &Path, previous: i32) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .is_some_and(|pid| pid != previous && alive(pid))
}

/// A failure after 60 seconds up resets the next delay to 1 second.
///
/// Tokio time is paused, so the minute of uptime is `advance`d while the
/// third process is still in a real `sleep`. A reset that did not happen
/// would wait 4 seconds, and the 1.5 second advance would not start it.
#[test]
fn backoff_resets_after_a_minute_up() {
    let _timing = timing_lock();
    run_local(async {
        tokio::time::pause();
        let tmp = TempDir::new().unwrap();
        let root = dir(tmp.path(), "plugin");
        let server = Running::one(
            &root,
            "n=0\n\
             if [ -f count ]; then n=$(cat count); fi\n\
             n=$((n + 1))\n\
             echo \"$n\" > count\n\
             echo $$ > live.pid\n\
             date +%s%N >> starts\n\
             if [ \"$n\" -le 2 ]; then exit 1; fi\n\
             if [ \"$n\" -eq 3 ]; then echo $$ > holdpid; sleep 1000; fi\n\
             echo hold >> starts\n\
             sleep 1000\n",
        );
        let starts = root.join("starts");
        let live = root.join("live.pid");
        wait_reaped_generation(&starts, &live, 1).await;
        tokio::time::advance(Duration::from_millis(1_100)).await;
        wait_reaped_generation(&starts, &live, 2).await;
        tokio::time::advance(Duration::from_millis(2_100)).await;
        let holdpid = root.join("holdpid");
        let mut held = None;
        let found = wait_real(Duration::from_secs(8), || {
            held = std::fs::read_to_string(&holdpid)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
                .filter(|pid| alive(*pid));
            held.is_some()
        })
        .await;
        assert!(
            found,
            "third start did not stay up: {:?}",
            read_lines(&starts)
        );
        let pid = held.expect("held pid");
        tokio::time::advance(Duration::from_secs(61)).await;
        assert!(alive(pid), "virtual time must not kill the live sidecar");
        unsafe { libc::kill(pid, libc::SIGTERM) };
        wait_reaped_generation(&starts, &live, 3).await;
        let held = || read_lines(&starts).iter().any(|line| line == "hold");
        assert!(
            !held(),
            "the reset restart must wait for the advanced backoff"
        );
        tokio::time::advance(Duration::from_millis(1500)).await;
        assert!(
            wait_real(Duration::from_secs(8), held).await,
            "backoff did not reset to 1s: {:?}",
            read_lines(&starts)
        );
        tokio::time::resume();
        server.stop().await;
    });
}

#[test]
fn clean_stop_signals_the_group() {
    let _timing = timing_lock();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let root = dir(tmp.path(), "plugin");
        let server = Running::one(
            &root,
            "trap 'echo termed > marker; exit 0' TERM\necho $$ > pid\nsleep 1000\n",
        );
        wait_file(&root.join("pid")).await;
        let pid = read_pid(&root.join("pid"));
        assert!(alive(pid));
        let started = Instant::now();
        server.stop().await;
        assert!(
            root.join("marker").is_file(),
            "sidecar should observe SIGTERM"
        );
        assert!(!alive(pid), "process group leader should be gone");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "a leader that exits on SIGTERM should not wait out the 5s grace: {elapsed:?}"
        );
    });
}

#[test]
fn three_stubborn_sidecars_share_one_five_second_deadline() {
    let _timing = timing_lock();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let roots = ["a", "b", "c"].map(|name| dir(tmp.path(), name));
        let admitted = roots
            .iter()
            .zip(["a", "b", "c"])
            .map(|(root, name)| {
                let command = sh(root, "trap '' TERM\necho $$ > pid\nsleep 1000\n");
                spec(root, name, command, 1)
            })
            .collect();
        let server = Running::boot(catalog(admitted), None, None);
        for root in &roots {
            wait_file(&root.join("pid")).await;
        }
        let pids = roots.map(|root| read_pid(&root.join("pid")));
        assert!(pids.iter().all(|pid| alive(*pid)));
        let started = Instant::now();
        server.stop().await;
        let elapsed = started.elapsed();
        assert!(
            elapsed > Duration::from_secs(4) && elapsed < Duration::from_secs(10),
            "server exit took {elapsed:?}; three SIGTERM-ignoring sidecars must share one 5s deadline"
        );
        assert!(pids.iter().all(|pid| !alive(*pid)));
    });
}

/// The leaders exit on `SIGTERM` immediately. One grandchild handles
/// `SIGTERM` and must be allowed to finish; another ignores it and is
/// killed only after the shared deadline. The stop stays inside that
/// deadline.
#[test]
fn stop_grace_covers_a_grandchild_after_the_leader_exits() {
    let _timing = timing_lock();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let honor = dir(tmp.path(), "honor");
        let ignore = dir(tmp.path(), "ignore");
        let grandchild = |trap: &str| {
            format!("trap 'exit 0' TERM\n{trap} &\necho $! > grand\necho $$ > leader\nwait\n")
        };
        let honor_cmd = sh(
            &honor,
            &grandchild("(trap 'sleep 1; touch done; exit 0' TERM; while :; do :; done)"),
        );
        let ignore_cmd = sh(&ignore, &grandchild("(trap '' TERM; sleep 1000)"));
        let server = Running::boot(
            catalog(vec![
                spec(&honor, "plug.honor", honor_cmd, 1),
                spec(&ignore, "plug.ignore", ignore_cmd, 2),
            ]),
            None,
            None,
        );
        wait_file(&honor.join("grand")).await;
        wait_file(&ignore.join("grand")).await;
        let ignore_grand = read_pid(&ignore.join("grand"));
        let started = Instant::now();
        server.stop().await;
        let elapsed = started.elapsed();
        assert!(
            honor.join("done").is_file(),
            "a TERM-honoring grandchild must finish during the shared grace"
        );
        assert!(
            !alive(ignore_grand),
            "a TERM-ignoring grandchild must be killed after the deadline"
        );
        assert!(
            elapsed > Duration::from_secs(4) && elapsed < Duration::from_secs(10),
            "stop took {elapsed:?}; the shared deadline is 5s"
        );
    });
}

#[test]
fn disable_and_manifest_change_reconcile_without_touching_unchanged() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let a = dir(tmp.path(), "a");
        let b = dir(tmp.path(), "b");
        let cmd_a = sh(&a, "echo $$ > pid\nsleep 1000\n");
        let cmd_b = sh(&b, "echo $$ > pid\nsleep 1000\n");
        let shared = Arc::new(Mutex::new(catalog(vec![
            spec(&a, "plug.a", cmd_a.clone(), 1),
            spec(&b, "plug.b", cmd_b, 1),
        ])));
        let source_catalog = Arc::clone(&shared);
        let source = PluginStartupSource::new(move || Ok(source_catalog.lock().unwrap().clone()));
        let initial = shared.lock().unwrap().clone();
        let server = Running::boot(initial, Some(source), None);
        wait_file(&a.join("pid")).await;
        wait_file(&b.join("pid")).await;
        let pid_a = read_pid(&a.join("pid"));
        let pid_b = read_pid(&b.join("pid"));
        shared
            .lock()
            .unwrap()
            .admitted
            .retain(|spec| spec.plugin_id == "plug.b");
        ring(&server).await;
        assert!(
            wait_until(Duration::from_secs(12), || !alive(pid_a)).await,
            "disabled sidecar should exit"
        );
        assert!(alive(pid_b), "unchanged sidecar must keep its pid");
        {
            let mut guard = shared.lock().unwrap();
            guard.admitted[0].manifest_hash = 2;
            guard.admitted.push(spec(&a, "plug.a", cmd_a, 1));
        }
        let _ = std::fs::remove_file(b.join("pid"));
        let _ = std::fs::remove_file(a.join("pid"));
        ring(&server).await;
        assert!(
            wait_until(Duration::from_secs(12), || {
                fresh_pid(&b.join("pid"), pid_b) && fresh_pid(&a.join("pid"), pid_a)
            })
            .await,
            "a changed manifest restarts that entry and enable starts the other"
        );
        server.stop().await;
    });
}

/// Ring the reload doorbell: a `CONFIG_RELOAD` metadata write.
async fn ring(server: &Running) {
    use phux_protocol::wire::frame::{CONFIG_RELOAD_KEY, Scope};
    use phux_server_testkit::{SOCKET_CONNECT_DEADLINE, send_frame, wait_for_socket};
    static NONCE: AtomicU32 = AtomicU32::new(0);
    let socket = server.tmp.path().join("phux.sock");
    let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
    let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
    send_frame(
        &mut stream,
        &phux_server_testkit::set_metadata(
            1,
            Scope::Global,
            CONFIG_RELOAD_KEY,
            format!("nonce-{}-{nonce}", std::process::id()).into_bytes(),
        ),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[test]
fn sidecar_connection_counts_toward_idle_exit() {
    let _timing = timing_lock();
    run_local(async {
        let holder = TempDir::new().unwrap();
        let script = holder.path().join("hold.py");
        std::fs::write(
            &script,
            "import os, socket, sys, time\n\
             s = socket.socket(socket.AF_UNIX)\n\
             deadline = time.time() + 3\n\
             while True:\n\
             \ttry:\n\
             \t\ts.connect(os.environ['PHUX_SOCKET'])\n\
             \t\tbreak\n\
             \texcept OSError:\n\
             \t\tif time.time() > deadline:\n\
             \t\t\traise\n\
             \t\ttime.sleep(0.05)\n\
             open(sys.argv[1], 'w').write('ok\\n')\n\
             time.sleep(1000)\n",
        )
        .unwrap();
        let root = dir(holder.path(), "plugin");
        let marker = root.join("connected");
        let command = vec![
            "python3".to_owned(),
            script.display().to_string(),
            marker.display().to_string(),
        ];
        let idle = Some(Duration::from_secs(4));
        let server = Running::boot(catalog(vec![spec(&root, "plug.a", command, 1)]), None, idle);
        wait_file(&marker).await;
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert!(
            !server.handle.as_ref().unwrap().is_finished(),
            "a connected sidecar must keep the server past the idle limit"
        );
        server.stop().await;

        let quiet = dir(holder.path(), "quiet");
        let sleep = sh(&quiet, "echo $$ > pid\nsleep 1000\n");
        let mut server = Running::boot(catalog(vec![spec(&quiet, "plug.a", sleep, 1)]), None, idle);
        wait_file(&quiet.join("pid")).await;
        // Leave the shutdown sender held so only idle-exit can end the server.
        let handle = server.handle.take().unwrap();
        let finished = timeout(Duration::from_secs(20), handle)
            .await
            .expect("unconnected sidecar must not hold the server open");
        finished.expect("join").expect("idle exit");
    });
}

#[test]
fn sidecar_env_cwd_umask_and_stdin() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let root = dir(tmp.path(), "plugin");
        let server = Running::one(
            &root,
            "pwd > cwd\n\
             umask > umask\n\
             env | sort > env\n\
             : > \"$PHUX_PLUGIN_STATE/ready\"\n\
             read line || echo eof > stdin\n\
             sleep 1000\n",
        );
        wait_file(&server.state.join("plugin-startup/plug.a/web/ready")).await;
        wait_file(&root.join("stdin")).await;
        let read = |name: &str| std::fs::read_to_string(root.join(name)).unwrap();
        assert_eq!(
            std::fs::canonicalize(read("cwd").trim()).unwrap(),
            std::fs::canonicalize(&root).unwrap()
        );
        assert!(read("umask").contains("0077"), "{}", read("umask"));
        let env = read("env");
        for key in [
            "PHUX_SOCKET=",
            "PHUX_PLUGIN_ID=plug.a",
            "PHUX_PLUGIN_NAME=plug.a",
            "PHUX_PLUGIN_VERSION=0.1.0",
            "PHUX_PLUGIN_ROOT=",
            "PHUX_PLUGIN_STATE=",
        ] {
            assert!(env.contains(key), "{key} missing from\n{env}");
        }
        for key in ["PHUX_TERMINAL_ID=", "PHUX_PANE=", "PHUX_SESSION="] {
            assert!(
                !env.lines().any(|line| line.starts_with(key)),
                "{key} leaked"
            );
        }
        assert_eq!(read("stdin").trim(), "eof");
        server.stop().await;
    });
}
