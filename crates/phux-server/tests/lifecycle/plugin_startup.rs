//! `[[startup]]` sidecars: restart, stop, cap, platform, reconcile, idle.

use std::fmt::Write as _;
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

fn spec(root: &Path, plugin: &str, id: &str, command: Vec<String>, hash: u64) -> StartupSpec {
    StartupSpec {
        plugin_id: plugin.to_owned(),
        plugin_name: plugin.to_owned(),
        plugin_version: "0.1.0".to_owned(),
        startup_id: id.to_owned(),
        command,
        plugin_root: root.to_path_buf(),
        manifest_hash: hash,
    }
}

fn sh(root: &Path, script: &str) -> Vec<String> {
    let path = root.join("run.sh");
    std::fs::write(&path, script).unwrap();
    vec!["/bin/sh".to_owned(), path.display().to_string()]
}

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
        let state = tmp.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
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

    async fn stop(mut self) {
        self.shutdown.take();
        let handle = self.handle.take().unwrap();
        timeout(Duration::from_secs(20), handle)
            .await
            .expect("server did not exit")
            .expect("join")
            .expect("server");
    }

    fn runs(&self) -> String {
        std::fs::read_to_string(self.state.join("plugin-runs.jsonl")).unwrap_or_default()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.shutdown.take();
        let root = self.state.join("plugin-startup");
        let Ok(plugins) = std::fs::read_dir(&root) else {
            return;
        };
        for plugin in plugins.flatten() {
            let Ok(files) = std::fs::read_dir(plugin.path()) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name();
                if !Path::new(&name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("pid"))
                {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(file.path()) else {
                    continue;
                };
                if let Some(pid) = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|value| value.get("pid").and_then(serde_json::Value::as_i64))
                {
                    unsafe { libc::kill(i32::try_from(pid).unwrap_or(0), libc::SIGKILL) };
                }
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
        if read_lines(starts).len() < n {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(live) else {
            return false;
        };
        let Ok(pid) = text.trim().parse::<i32>() else {
            return false;
        };
        !alive(pid)
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

fn nanos(line: &str) -> u128 {
    line.trim().parse().unwrap_or(0)
}

/// Crash: exit 1 restarts with the backoff steps, and the sixth failure stays dead.
#[test]
fn crash_restarts_with_backoff_then_gives_up() {
    let _timing = timing_lock();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        let command = sh(
            &root,
            "n=0\n\
             if [ -f count ]; then n=$(cat count); fi\n\
             n=$((n + 1))\n\
             echo \"$n\" > count\n\
             date +%s%N >> starts\n\
             exit 1\n",
        );
        let server = Running::boot(
            StartupCatalog {
                admitted: vec![spec(&root, "plug.a", "web", command, 1)],
                skipped_cap: Vec::new(),
            },
            None,
            None,
        );
        let starts = root.join("starts");
        assert!(
            wait_until(Duration::from_secs(90), || read_lines(&starts).len() >= 6).await,
            "expected 6 starts, saw {:?}",
            read_lines(&starts)
        );
        let stamps: Vec<u128> = read_lines(&starts)
            .into_iter()
            .map(|line| nanos(&line))
            .collect();
        let gaps_ms = [1_000_u128, 2_000, 4_000, 8_000, 16_000];
        for (index, expected) in gaps_ms.iter().enumerate() {
            let gap = stamps[index + 1].saturating_sub(stamps[index]) / 1_000_000;
            assert!(
                gap + 300 >= *expected,
                "gap {index} was {gap}ms, expected at least {expected}ms"
            );
            assert!(
                gap < expected + 8_000,
                "gap {index} was {gap}ms, expected under {}ms",
                expected + 8_000
            );
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(
            read_lines(&starts).len(),
            6,
            "the sixth failure must stay dead"
        );
        assert!(server.runs().contains("gave up"), "{}", server.runs());
        server.stop().await;
    });
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
        let root = tmp.path().join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        let command = sh(
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
        let server = Running::boot(
            StartupCatalog {
                admitted: vec![spec(&root, "plug.a", "web", command, 1)],
                skipped_cap: Vec::new(),
            },
            None,
            None,
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
            "third start did not stay up: {:?} hold={:?}",
            read_lines(&starts),
            std::fs::read_to_string(&holdpid)
        );
        let pid = held.expect("held pid");
        tokio::time::advance(Duration::from_secs(61)).await;
        assert!(alive(pid), "virtual time must not kill the live sidecar");
        unsafe { libc::kill(pid, libc::SIGTERM) };
        wait_reaped_generation(&starts, &live, 3).await;
        assert!(
            !read_lines(&starts).iter().any(|line| line == "hold"),
            "the reset restart must wait for the advanced backoff"
        );
        tokio::time::advance(Duration::from_millis(1500)).await;
        assert!(
            wait_real(Duration::from_secs(8), || {
                read_lines(&starts).iter().any(|line| line == "hold")
            })
            .await,
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
        let root = tmp.path().join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        let command = sh(
            &root,
            "trap 'echo termed > marker; exit 0' TERM\n\
             echo $$ > pid\n\
             sleep 1000\n",
        );
        let server = Running::boot(
            StartupCatalog {
                admitted: vec![spec(&root, "plug.a", "web", command, 1)],
                skipped_cap: Vec::new(),
            },
            None,
            None,
        );
        assert!(wait_until(Duration::from_secs(10), || root.join("pid").is_file()).await);
        let pid: i32 = std::fs::read_to_string(root.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(alive(pid));
        let started = Instant::now();
        server.stop().await;
        assert!(
            root.join("marker").is_file(),
            "sidecar should observe SIGTERM"
        );
        assert!(!alive(pid), "process group leader should be gone");
        assert!(started.elapsed() < Duration::from_secs(10));
    });
}

#[test]
fn three_stubborn_sidecars_share_one_five_second_deadline() {
    let _timing = timing_lock();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let mut admitted = Vec::new();
        for name in ["a", "b", "c"] {
            let root = tmp.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            let command = sh(
                &root,
                "trap '' TERM\n\
                 echo $$ > pid\n\
                 sleep 1000\n",
            );
            admitted.push(spec(&root, name, "web", command, 1));
        }
        let server = Running::boot(
            StartupCatalog {
                admitted,
                skipped_cap: Vec::new(),
            },
            None,
            None,
        );
        let pids = ["a", "b", "c"].map(|name| tmp.path().join(name).join("pid"));
        assert!(
            wait_until(Duration::from_secs(10), || pids
                .iter()
                .all(|path| path.is_file()))
            .await
        );
        let pids: Vec<i32> = pids
            .iter()
            .map(|path| {
                std::fs::read_to_string(path)
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap()
            })
            .collect();
        assert!(pids.iter().all(|pid| alive(*pid)));
        let started = Instant::now();
        server.stop().await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "server exit took {elapsed:?}; three SIGTERM-ignoring sidecars must share one 5s deadline"
        );
        assert!(
            elapsed > Duration::from_secs(4),
            "grace was shorter than 4s: {elapsed:?}"
        );
        assert!(pids.iter().all(|pid| !alive(*pid)));
    });
}

#[test]
fn a_grandchild_dies_with_the_group() {
    let _timing = timing_lock();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        let command = sh(
            &root,
            "sleep 1000 &\n\
             echo $! > grand\n\
             echo $$ > leader\n\
             wait\n",
        );
        let server = Running::boot(
            StartupCatalog {
                admitted: vec![spec(&root, "plug.a", "web", command, 1)],
                skipped_cap: Vec::new(),
            },
            None,
            None,
        );
        assert!(wait_until(Duration::from_secs(10), || root.join("grand").is_file()).await);
        let leader: i32 = std::fs::read_to_string(root.join("leader"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let grand: i32 = std::fs::read_to_string(root.join("grand"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(alive(leader) && alive(grand));
        server.stop().await;
        assert!(!alive(leader), "leader should die with the server");
        assert!(!alive(grand), "grandchild should die with the group");
    });
}

#[test]
fn exit_zero_is_not_restarted() {
    let _timing = timing_lock();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        let command = sh(&root, "echo hi >> out\nexit 0\n");
        let server = Running::boot(
            StartupCatalog {
                admitted: vec![spec(&root, "plug.a", "web", command, 1)],
                skipped_cap: Vec::new(),
            },
            None,
            None,
        );
        assert!(wait_until(Duration::from_secs(10), || root.join("out").is_file()).await);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(read_lines(&root.join("out")), ["hi"]);
        assert!(
            server.runs().contains("exited 0 (not restarting)"),
            "{}",
            server.runs()
        );
        server.stop().await;
    });
}

#[test]
fn platform_filter_skips_other_operating_systems() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let this = if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "windows"
        };
        let other = if this == "linux" { "windows" } else { "linux" };
        let ok = tmp.path().join("ok");
        let no = tmp.path().join("no");
        for dir in [&ok, &no] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let body = |id: &str, platforms: &str| {
            format!(
                "id = \"{id}\"\nname = \"{id}\"\nversion = \"0.1.0\"\nmin_phux_version = \"0.0.2\"\n\
                 [[startup]]\nid = \"web\"\nplatforms = [\"{platforms}\"]\ncommand = [\"/bin/sh\", \"-c\", \"echo $$ > pid; sleep 1000\"]\n"
            )
        };
        std::fs::write(ok.join("phux-plugin.toml"), body("plug.ok", this)).unwrap();
        std::fs::write(no.join("phux-plugin.toml"), body("plug.no", other)).unwrap();
        let plugins = vec![
            phux_config::plugin::PluginConfigEntry {
                manifest: ok.join("phux-plugin.toml"),
                enabled: true,
            },
            phux_config::plugin::PluginConfigEntry {
                manifest: no.join("phux-plugin.toml"),
                enabled: true,
            },
        ];
        let catalog = phux_server::plugin_startup::catalog_from_plugins(tmp.path(), &plugins);
        assert_eq!(catalog.admitted.len(), 1);
        assert_eq!(catalog.admitted[0].plugin_id, "plug.ok");
        let server = Running::boot(catalog, None, None);
        assert!(wait_until(Duration::from_secs(10), || ok.join("pid").is_file()).await);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            !no.join("pid").exists(),
            "other-platform sidecar must not spawn"
        );
        server.stop().await;
    });
}

#[test]
fn global_cap_skips_later_plugins_in_order() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let mut plugins = Vec::new();
        let mut roots = Vec::new();
        for (plugin, count) in [("plug.a", 5), ("plug.b", 4)] {
            let root = tmp.path().join(plugin);
            std::fs::create_dir_all(&root).unwrap();
            let mut body = format!(
                "id = \"{plugin}\"\nname = \"{plugin}\"\nversion = \"0.1.0\"\nmin_phux_version = \"0.0.2\"\n"
            );
            for n in 0..count {
                let id = format!("{plugin}{n}");
                let dir = root.join(&id);
                std::fs::create_dir_all(&dir).unwrap();
                let pid_path = dir.join("pid");
                std::fs::write(
                    dir.join("run.sh"),
                    format!("echo $$ > {}\nsleep 1000\n", pid_path.display()),
                )
                .unwrap();
                let _ = write!(
                    body,
                    "[[startup]]\nid = \"s{n}\"\ncommand = [\"/bin/sh\", \"{}\"]\n",
                    dir.join("run.sh").display()
                );
                roots.push((plugin.to_owned(), format!("s{n}"), dir));
            }
            let manifest = root.join("phux-plugin.toml");
            std::fs::write(&manifest, body).unwrap();
            plugins.push(phux_config::plugin::PluginConfigEntry {
                manifest,
                enabled: true,
            });
        }
        let catalog = phux_server::plugin_startup::catalog_from_plugins(tmp.path(), &plugins);
        assert_eq!(catalog.skipped_cap.len(), 1);
        assert_eq!(catalog.skipped_cap[0].startup_id, "s3");
        assert_eq!(catalog.skipped_cap[0].plugin_id, "plug.b");
        let server = Running::boot(catalog, None, None);
        let admitted: Vec<_> = roots
            .iter()
            .take(8)
            .map(|(_, _, dir)| dir.join("pid"))
            .collect();
        assert!(
            wait_until(Duration::from_secs(15), || admitted
                .iter()
                .all(|path| path.is_file()))
            .await,
            "admitted sidecars did not all start"
        );
        let skipped = &roots[8].2;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            !skipped.join("pid").exists(),
            "the 9th entry must not start"
        );
        assert!(server.runs().contains("skipped: cap"), "{}", server.runs());
        assert!(server.runs().contains("plug.b"), "{}", server.runs());
        server.stop().await;
    });
}

#[test]
fn disable_and_manifest_change_reconcile_without_touching_unchanged() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let cmd_a = sh(&a, "echo $$ > pid\nsleep 1000\n");
        let cmd_b = sh(&b, "echo $$ > pid\nsleep 1000\n");
        let catalog = Arc::new(Mutex::new(StartupCatalog {
            admitted: vec![
                spec(&a, "plug.a", "web", cmd_a.clone(), 1),
                spec(&b, "plug.b", "web", cmd_b.clone(), 1),
            ],
            skipped_cap: Vec::new(),
        }));
        let source_catalog = Arc::clone(&catalog);
        let source = PluginStartupSource::new(move || Ok(source_catalog.lock().unwrap().clone()));
        let initial = catalog.lock().unwrap().clone();
        let server = Running::boot(initial, Some(source), None);
        assert!(
            wait_until(Duration::from_secs(10), || a.join("pid").is_file()
                && b.join("pid").is_file())
            .await
        );
        let pid_a: i32 = std::fs::read_to_string(a.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let pid_b: i32 = std::fs::read_to_string(b.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        catalog
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
            let mut guard = catalog.lock().unwrap();
            guard.admitted[0].manifest_hash = 2;
            guard
                .admitted
                .push(spec(&a, "plug.a", "web", cmd_a.clone(), 1));
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

async fn ring(server: &Running) {
    // The doorbell is a metadata write. Drive it by replacing the catalog
    // the source already closes over and asking the supervisor directly is
    // not available here, so connect and send CONFIG_RELOAD.
    use phux_protocol::wire::frame::{CONFIG_RELOAD_KEY, FrameKind, Scope};
    use phux_server_testkit::{SOCKET_CONNECT_DEADLINE, send_frame, wait_for_socket};
    let socket = server.tmp.path().join("phux.sock");
    let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::SetMetadata {
            request_id: 1,
            scope: Scope::Global,
            key: CONFIG_RELOAD_KEY.to_owned(),
            value: format!(
                "nonce-{}-{}",
                std::process::id(),
                NEXUS.fetch_add(1, Ordering::Relaxed)
            )
            .into_bytes(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(stream);
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
        let root = holder.path().join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("connected");
        let command = vec![
            "python3".to_owned(),
            script.display().to_string(),
            marker.display().to_string(),
        ];
        let server = Running::boot(
            StartupCatalog {
                admitted: vec![spec(&root, "plug.a", "web", command, 1)],
                skipped_cap: Vec::new(),
            },
            None,
            Some(Duration::from_secs(4)),
        );
        assert!(
            wait_until(Duration::from_secs(10), || marker.is_file()).await,
            "sidecar did not connect"
        );
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert!(
            !server.handle.as_ref().unwrap().is_finished(),
            "a connected sidecar must keep the server past the idle limit"
        );
        server.stop().await;

        let quiet = holder.path().join("quiet");
        std::fs::create_dir_all(&quiet).unwrap();
        let sleep = sh(&quiet, "echo $$ > pid\nsleep 1000\n");
        let mut server = Running::boot(
            StartupCatalog {
                admitted: vec![spec(&quiet, "plug.a", "web", sleep, 1)],
                skipped_cap: Vec::new(),
            },
            None,
            Some(Duration::from_secs(4)),
        );
        assert!(wait_until(Duration::from_secs(10), || quiet.join("pid").is_file()).await);
        // Leave the shutdown sender held so only idle-exit can end the server.
        let handle = server.handle.take().unwrap();
        let finished = timeout(Duration::from_secs(20), handle)
            .await
            .expect("unconnected sidecar must not hold the server open");
        finished.expect("join").expect("idle exit");
    });
}

static NEXUS: AtomicU32 = AtomicU32::new(0);

#[test]
fn sidecar_env_cwd_umask_and_stdin() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        let command = sh(
            &root,
            "pwd > cwd\n\
             umask > umask\n\
             env | sort > env\n\
             : > \"$PHUX_PLUGIN_STATE/ready\"\n\
             read line || echo eof > stdin\n\
             sleep 1000\n",
        );
        let server = Running::boot(
            StartupCatalog {
                admitted: vec![spec(&root, "plug.a", "web", command, 1)],
                skipped_cap: Vec::new(),
            },
            None,
            None,
        );
        let ready = server
            .state
            .join("plugin-startup")
            .join("plug.a")
            .join("web")
            .join("ready");
        assert!(
            wait_until(Duration::from_secs(10), || {
                ready.is_file() && root.join("stdin").is_file()
            })
            .await
        );
        let cwd = std::fs::read_to_string(root.join("cwd")).unwrap();
        assert_eq!(
            std::fs::canonicalize(cwd.trim()).unwrap(),
            std::fs::canonicalize(&root).unwrap()
        );
        let umask = std::fs::read_to_string(root.join("umask")).unwrap();
        assert!(umask.contains("0077"), "{umask}");
        let env = std::fs::read_to_string(root.join("env")).unwrap();
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
        assert!(
            !env.lines()
                .any(|line| line.starts_with("PHUX_TERMINAL_ID="))
        );
        assert!(!env.lines().any(|line| line.starts_with("PHUX_PANE=")));
        assert!(!env.lines().any(|line| line.starts_with("PHUX_SESSION=")));
        assert_eq!(
            std::fs::read_to_string(root.join("stdin")).unwrap().trim(),
            "eof"
        );
        server.stop().await;
    });
}
