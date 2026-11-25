//! Several hub consumers of one satellite pane: each draws, the link stays
//! up, a redial draws every viewer again, and no generation identity is ever
//! offered twice to one consumer.

use std::collections::HashSet;

use super::*;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};

type Generation = (StreamId, BootstrapId);

/// A hub consumer that publishes bootstraps by the client kernel's rules: a
/// retired or already seen generation is never drawn again.
struct Viewer {
    stream: UnixStream,
    pane: ResourceId,
    next_request: u32,
    staging: Option<Generation>,
    published: Option<Generation>,
    retired: HashSet<Generation>,
    unreachable: u32,
}

impl Viewer {
    fn new(stream: UnixStream, pane: &ResourceId) -> Self {
        Self {
            stream,
            pane: pane.clone(),
            next_request: 7000,
            staging: None,
            published: None,
            retired: HashSet::new(),
            unreachable: 0,
        }
    }

    fn apply(&mut self, frame: &FrameKind) {
        match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } if *terminal_id == self.pane => {
                let generation = (*stream_id, *bootstrap_id);
                let known = self.retired.contains(&generation)
                    || self.published == Some(generation)
                    || self.staging == Some(generation);
                if !known && let Some(replaced) = self.staging.replace(generation) {
                    self.retired.insert(replaced);
                }
            }
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } if *terminal_id == self.pane => {
                let generation = (*stream_id, *bootstrap_id);
                if self.staging == Some(generation)
                    && let Some(replaced) = self.published.replace(generation)
                {
                    self.retired.insert(replaced);
                }
                self.staging.take_if(|staging| *staging == generation);
            }
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } if *terminal_id == self.pane => {
                self.retired.insert((*stream_id, *bootstrap_id));
            }
            FrameKind::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            } => self.unreachable += 1,
            _ => {}
        }
    }

    fn drawn(&self) -> Option<Generation> {
        self.published
            .filter(|generation| !self.retired.contains(generation))
    }

    async fn command(&mut self, command: Command) {
        self.next_request += 1;
        let (result, frames) = command_via_hub(&mut self.stream, self.next_request, command).await;
        assert_eq!(result, CommandResult::Ok);
        for frame in &frames {
            self.apply(frame);
        }
    }

    async fn attach(&mut self) {
        self.command(Command::attach_terminal(self.pane.clone(), None))
            .await;
    }

    async fn detach(&mut self) {
        self.command(Command::DetachResource {
            terminal_id: self.pane.clone(),
        })
        .await;
    }

    /// Resize, and wait for the replacement generation.
    async fn resize(&mut self, cols: u16, rows: u16) {
        let before = self.drawn();
        self.send_resize(cols, rows).await;
        self.read_until("the resized generation", |viewer, _| {
            viewer.drawn().is_some() && viewer.drawn() != before
        })
        .await;
    }

    async fn send_resize(&mut self, cols: u16, rows: u16) {
        let resize = FrameKind::ResizeTerminal {
            terminal_id: self.pane.clone(),
            cols,
            rows,
            cell_px: None,
        };
        send_frame(&mut self.stream, &resize).await;
    }

    async fn paste(&mut self, text: &str) {
        let paste = PasteEvent {
            trust: PasteTrust::Trusted,
            data: format!("{text}\n").into_bytes(),
        };
        let input = InputEvent::Paste(paste).into_frame(self.pane.clone());
        send_frame(&mut self.stream, &input).await;
    }

    async fn read_until(&mut self, what: &str, done: impl Fn(&Self, Option<&FrameKind>) -> bool) {
        let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
        if done(self, None) {
            return;
        }
        loop {
            let read = tokio::time::timeout_at(deadline, recv_typed(&mut self.stream)).await;
            let Ok((_, frame)) = read else {
                panic!("timed out waiting for {what}: drawn {:?}", self.drawn());
            };
            self.apply(&frame);
            if done(self, Some(&frame)) {
                return;
            }
        }
    }

    async fn see_drawn(&mut self) {
        self.read_until("a drawn pane", |viewer, _| viewer.drawn().is_some())
            .await;
    }

    /// `text`, as live output of the drawn generation or in one it stages.
    async fn see_output(&mut self, text: &'static str) {
        self.read_until(text, |viewer, frame| {
            let (expected, named, bytes) = match frame {
                Some(FrameKind::ResourceOutput {
                    stream_id,
                    bootstrap_id,
                    bytes,
                    ..
                }) => (viewer.drawn(), (*stream_id, *bootstrap_id), bytes),
                Some(FrameKind::BootstrapChunk {
                    stream_id,
                    bootstrap_id,
                    payload,
                    ..
                }) => (viewer.staging, (*stream_id, *bootstrap_id), payload),
                _ => return false,
            };
            expected == Some(named)
                && bytes
                    .windows(text.len())
                    .any(|window| window == text.as_bytes())
        })
        .await;
    }
}

fn echoing_seed() -> LaunchCommand {
    LaunchCommand::new("/bin/cat")
}

/// Two hub viewers of one pane at different sizes both draw, typing from
/// either reaches the satellite pane, and the link never drops.
#[test]
fn a_second_viewer_draws_and_the_link_survives() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot_with(Some(echoing_seed()), Some("local")).await;
        let pane = fed.sat_id();
        let mut a = Viewer::new(fed.linked_hub().await, &pane);
        let mut b = Viewer::new(fed.hub().await, &pane);
        a.attach().await;
        a.resize(110, 35).await;

        b.attach().await;
        b.see_drawn().await;
        b.send_resize(47, 32).await;
        for round in 0..4 {
            b.detach().await;
            b.attach().await;
            b.see_drawn().await;
            b.send_resize(47 + round, 32).await;
        }
        b.paste("typed-by-b").await;
        b.see_output("typed-by-b").await;
        a.see_output("typed-by-b").await;
        a.paste("typed-by-a").await;
        a.see_output("typed-by-a").await;
        b.see_output("typed-by-a").await;
        assert_eq!((a.unreachable, b.unreachable), (0, 0), "the link dropped");

        drop((a, b));
        fed.shutdown().await;
    });
}

/// A loopback TCP hop between hub and satellite that [`Self::cut`] severs;
/// it carries the next connection at once, as a network that came back does.
struct Hop {
    port: u16,
    /// The satellite's port; sending one severs what is connected.
    target: tokio::sync::watch::Sender<u16>,
}

impl Hop {
    async fn to(target: u16) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, seen) = tokio::sync::watch::channel(target);
        tokio::task::spawn_local(async move {
            while let Ok((mut near, _)) = listener.accept().await {
                let mut seen = seen.clone();
                let target = *seen.borrow_and_update();
                tokio::task::spawn_local(async move {
                    let mut far = TcpStream::connect(("127.0.0.1", target)).await.unwrap();
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut near, &mut far) => {}
                        _ = seen.changed() => {}
                    }
                });
            }
        });
        Self {
            port,
            target: sender,
        }
    }

    fn cut(&self) {
        let port = *self.target.borrow();
        self.cut_over_to(port);
    }

    fn cut_over_to(&self, port: u16) {
        self.target.send_replace(port);
    }
}

/// A link that drops and redials draws every viewer again without any of them
/// asking; one that left meanwhile stays gone, and typing still lands.
#[test]
fn a_redial_draws_every_viewer_again() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let (sat_port, sat_shutdown, sat_task) =
            spawn_satellite_runtime(tmp.path().join("sat.sock"), Some(echoing_seed())).await;
        let seed = discover_satellite_pane(sat_port).await;
        let pane = ResourceId::satellite("sat", seed);
        let hop = Hop::to(sat_port).await;
        let hub_path = tmp.path().join("hub.sock");
        let (hub_shutdown, hub_task) =
            spawn_hub(hub_path.clone(), vec![satellite_entry("sat", hop.port)]);
        let mut first = hub_client(&hub_path).await;
        get_screen_until_ok(&mut first, seed).await;

        let mut viewers = [
            Viewer::new(first, &pane),
            Viewer::new(hub_client(&hub_path).await, &pane),
        ];
        let mut gone = Viewer::new(hub_client(&hub_path).await, &pane);
        for viewer in viewers.iter_mut().chain([&mut gone]) {
            viewer.attach().await;
            viewer.see_drawn().await;
        }
        hop.cut();
        gone.read_until("the link loss", |viewer, _| viewer.unreachable > 0)
            .await;
        drop(gone);
        for viewer in &mut viewers {
            viewer
                .read_until("the link loss", |viewer, _| viewer.unreachable > 0)
                .await;
            let before = viewer.drawn();
            viewer
                .read_until("a fresh bootstrap after the redial", |viewer, _| {
                    viewer.drawn().is_some() && viewer.drawn() != before
                })
                .await;
            assert_eq!(viewer.unreachable, 1, "one notice, one redial");
        }
        let [a, b] = &mut viewers;
        a.paste("after-redial").await;
        a.see_output("after-redial").await;
        b.see_output("after-redial").await;

        drop(viewers);
        stop(hub_shutdown, hub_task).await;
        stop(sat_shutdown, sat_task).await;
    });
}

/// Prints until told otherwise, in more frames than a stalled consumer's
/// mailbox, socket and the hub's retention hold, then marks the end.
const FLOOD: &str = "read go; i=0; while [ $i -lt 200 ]; do printf '%0999d\\n' $i; \
     sleep 0.01; i=$((i+1)); done; echo FLOOD-OVER; exec cat";

/// A consumer that stops reading while its pane prints (a phone on a bad
/// link) overflows, and still ends on the pane as it is once it reads. The
/// overflow used to end its stream with nothing to say so.
#[test]
fn a_consumer_that_stalled_is_drawn_once_it_reads() {
    phux_server_testkit::run_local(async {
        let mut seed = LaunchCommand::new("/bin/sh");
        seed.args(["-c", FLOOD]);
        let fed = Fed::boot_with(Some(seed), None).await;
        let mut stalled = Viewer::new(fed.linked_hub().await, &fed.sat_id());
        let mut reader = Viewer::new(fed.hub().await, &fed.sat_id());
        for viewer in [&mut stalled, &mut reader] {
            viewer.attach().await;
            viewer.see_drawn().await;
        }
        reader.paste("go").await;
        reader.see_output("FLOOD-OVER").await;
        stalled.see_output("FLOOD-OVER").await;

        drop((stalled, reader));
        fed.shutdown().await;
    });
}

/// A link that comes back to another satellite process carries no typing from
/// a consumer attached under the old one: the same id names another pane, and
/// another consumer's attach there would let the link's input through.
#[test]
fn a_restarted_satellite_gets_no_typing_meant_for_its_predecessor() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let (old_port, old_shutdown, old_task) =
            spawn_satellite_runtime(tmp.path().join("old.sock"), Some(echoing_seed())).await;
        let seed = discover_satellite_pane(old_port).await;
        let hop = Hop::to(old_port).await;
        let hub_path = tmp.path().join("hub.sock");
        let (hub_shutdown, hub_task) =
            spawn_hub(hub_path.clone(), vec![satellite_entry("sat", hop.port)]);
        let mut viewer = Viewer::new(
            hub_client(&hub_path).await,
            &ResourceId::satellite("sat", seed),
        );
        get_screen_until_ok(&mut viewer.stream, seed).await;
        viewer.attach().await;
        viewer.paste("to-the-old").await;
        viewer.see_output("to-the-old").await;

        let (new_port, new_shutdown, new_task) =
            spawn_satellite_runtime(tmp.path().join("new.sock"), Some(echoing_seed())).await;
        assert_eq!(discover_satellite_pane(new_port).await, seed, "id reused");
        hop.cut_over_to(new_port);
        viewer
            .read_until("the link loss", |viewer, _| viewer.unreachable > 0)
            .await;
        let mut other = Viewer::new(
            hub_client(&hub_path).await,
            &ResourceId::satellite("sat", seed),
        );
        get_screen_until_ok(&mut other.stream, seed).await;
        other.attach().await;
        viewer.paste("stray").await;
        // Attached again, the same consumer types into it; its stray did not.
        viewer.attach().await;
        viewer.paste("meant").await;
        viewer.see_output("meant").await;
        let screen = get_screen_via_hub(&mut other.stream, 1, other.pane.clone()).await;
        let CommandResult::OkWith(CommandValue::Json(screen)) = screen else {
            panic!("{screen:?}");
        };
        assert!(
            screen.contains("meant") && !screen.contains("stray"),
            "{screen}"
        );

        drop((viewer, other));
        stop(hub_shutdown, hub_task).await;
        stop(old_shutdown, old_task).await;
        stop(new_shutdown, new_task).await;
    });
}

/// One connection straight to a server: an attach is never given an id that
/// connection has seen, whatever its pumps resynced to and whether or not it
/// detached its session. No hub renames or repeats anything here.
#[test]
fn a_direct_reattach_never_reuses_a_generation() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot_with(Some(echoing_seed()), None).await;
        let stream = hub_client(&fed.tmp.path().join("sat.sock")).await;
        let mut viewer = Viewer::new(stream, &ResourceId::local(fed.seed));
        viewer.attach().await;
        for (cols, rows) in [(47, 32), (47, 20), (47, 32)] {
            viewer.resize(cols, rows).await;
        }
        for detach_session in [false, true, false] {
            let before = viewer.drawn();
            viewer.detach().await;
            if detach_session {
                send_frame(&mut viewer.stream, &FrameKind::Detach).await;
            }
            viewer.attach().await;
            assert!(
                viewer.drawn().is_some() && viewer.drawn() != before,
                "offered a generation already retired"
            );
            viewer.resize(50, 20).await;
            viewer.resize(50, 21).await;
        }

        drop(viewer);
        fed.shutdown().await;
    });
}

/// One consumer that resizes, leaves and comes back gets a generation it has
/// not retired: the satellite once handed out an id its own resyncs had used,
/// and an idle pane stayed blank.
#[test]
fn a_reopened_pane_never_reuses_a_generation() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot_with(Some(echoing_seed()), None).await;
        let mut viewer = Viewer::new(fed.linked_hub().await, &fed.sat_id());
        viewer.attach().await;
        for (cols, rows) in [(47, 32), (47, 20), (47, 32)] {
            viewer.resize(cols, rows).await;
        }
        for _ in 0..3 {
            let before = viewer.drawn();
            viewer.detach().await;
            viewer.attach().await;
            viewer
                .read_until("the reopened generation", |viewer, _| {
                    viewer.drawn().is_some() && viewer.drawn() != before
                })
                .await;
        }
        // A client that reconnects keeps what it retired, and the hub's link,
        // whose stream id it sees, is still the same one.
        for (cols, rows) in [(50, 30), (50, 20)] {
            viewer.resize(cols, rows).await;
        }
        let before = viewer.drawn();
        viewer.stream = fed.hub().await;
        viewer.attach().await;
        viewer
            .read_until("the generation after a reconnect", |viewer, _| {
                viewer.drawn().is_some() && viewer.drawn() != before
            })
            .await;
        assert_eq!(viewer.unreachable, 0);

        drop(viewer);
        fed.shutdown().await;
    });
}
