//! The `Command` enum every verb is declared in, one `Args` struct per
//! verb in the modules below, grouped by the help heading they list under.

mod agents;
mod attach;
mod machines;
mod maintain;
mod more;
mod options;
mod pane_input;
mod pane_read;
mod panes;
mod project;
mod recording;
mod server;
mod sessions;
mod settings;

pub(crate) use self::agents::*;
pub(crate) use self::attach::*;
pub(crate) use self::machines::*;
pub(crate) use self::maintain::*;
pub(crate) use self::more::*;
pub(crate) use self::options::*;
pub(crate) use self::pane_input::*;
pub(crate) use self::pane_read::*;
pub(crate) use self::panes::*;
pub(crate) use self::project::*;
pub(crate) use self::recording::*;
pub(crate) use self::server::*;
pub(crate) use self::sessions::*;
pub(crate) use self::settings::*;

use usage::Subcommands;

use crate::commands::play;

#[derive(Debug, Subcommands)]
pub(crate) enum Command {
    #[usage(help_heading = "More", display_order = 64)]
    RuntimeInfo(RuntimeInfoArgs),
    #[usage(alias = "a")]
    #[usage(help_heading = "Sessions", display_order = 10)]
    Attach(AttachArgs),

    #[usage(help_heading = "Machines", display_order = 41)]
    Server(ServerArgs),

    #[usage(alias = "list")]
    #[usage(help_heading = "Sessions", display_order = 12)]
    Ls(LsArgs),

    #[usage(help_heading = "More", display_order = 63)]
    Whoami(WhoamiArgs),

    #[usage(help_heading = "Maintain", display_order = 50)]
    Status(StatusArgs),

    #[usage(help_heading = "More", display_order = 62)]
    Perf(PerfArgs),

    #[usage(help_heading = "Sessions", display_order = 11)]
    New(NewArgs),

    #[usage(help_heading = "Panes", display_order = 20)]
    Spawn(SpawnArgs),

    #[usage(help_heading = "Agents", display_order = 31)]
    Launch(LaunchArgs),

    #[usage(help_heading = "Sessions", display_order = 13)]
    Kill(KillArgs),

    #[usage(name = "insert-pane")]
    #[usage(help_heading = "Panes", display_order = 29)]
    InsertPane(InsertPaneArgs),

    #[usage(help_heading = "Panes", display_order = 30)]
    Mv(MvArgs),

    #[usage(name = "swap-pane")]
    #[usage(help_heading = "Panes", display_order = 31)]
    SwapPane(SwapPaneArgs),

    #[usage(help_heading = "Panes", display_order = 27)]
    Resize(ResizeArgs),

    #[usage(help_heading = "Sessions", display_order = 14)]
    Detach(DetachArgs),

    #[usage(help_heading = "Agents", display_order = 33)]
    Take(TakeArgs),

    #[usage(help_heading = "Agents", display_order = 34)]
    Give(GiveArgs),

    #[usage(help_heading = "Agents", display_order = 35)]
    Signal(SignalArgs),

    #[usage(help_heading = "Agents", display_order = 36)]
    Mission(MissionArgs),

    #[usage(help_heading = "Agents", display_order = 37)]
    Mail(MailArgs),

    #[usage(help_heading = "Agents", display_order = 38)]
    Claim(ClaimArgs),

    #[usage(help_heading = "Agents", display_order = 71)]
    Approvals(ApprovalsArgs),

    #[usage(help_heading = "Agents", display_order = 72)]
    Approve(ApproveArgs),

    #[usage(help_heading = "Agents", display_order = 73)]
    Deny(DenyArgs),

    #[usage(help_heading = "Maintain", display_order = 56)]
    Update(UpdateArgs),

    #[usage(help_heading = "Maintain", display_order = 58)]
    Channel(ChannelArgs),

    #[usage(help_heading = "Maintain", display_order = 57)]
    Upgrade(UpgradeArgs),

    #[usage(help_heading = "Sessions", display_order = 15)]
    Rename(RenameArgs),

    #[usage(help_heading = "Sessions", display_order = 17)]
    Rm(RmArgs),

    #[usage(help_heading = "Sessions", display_order = 18)]
    Relaunch(RelaunchArgs),

    #[usage(help_heading = "Sessions", display_order = 19)]
    Restore(RestoreArgs),

    #[usage(help_heading = "Sessions", display_order = 20)]
    Organize(OrganizeArgs),

    #[usage(help_heading = "Panes", display_order = 21)]
    Snapshot(SnapshotArgs),

    #[usage(name = "send-keys")]
    #[usage(help_heading = "Panes", display_order = 22)]
    SendKeys(SendKeysArgs),

    #[usage(help_heading = "Panes", display_order = 24)]
    Upload(UploadArgs),

    #[usage(help_heading = "Machines", display_order = 46)]
    Download(DownloadArgs),

    #[usage(help_heading = "Panes", display_order = 23)]
    Paste(PasteArgs),

    #[usage(help_heading = "Panes", display_order = 25)]
    Wait(WaitArgs),

    #[usage(help_heading = "Panes", display_order = 26)]
    Watch(WatchArgs),

    #[usage(help_heading = "Panes", display_order = 32)]
    Resource(ResourceArgs),

    #[usage(help_heading = "More", display_order = 60)]
    Rec(RecArgs),

    /// Play a recording back as a live pane.
    #[usage(
        help = "Play a recording back as a live pane",
        long_help = "Play a recording back as a live pane.\n\n\
            Creates a new Terminal whose PTY is fed from FILE, then prints its id. The \
            result is an ordinary pane: attach it, `phux snapshot` it, `phux resize` it, \
            watch it from an agent, or `phux kill` it. It is not a viewer for your own \
            shell — for that, `asciinema play FILE` is the right tool and needs no server.\n\n\
            TARGET says WHERE the pane goes: the playback pane is created beside it, \
            splitting its window. TARGET is never written to, and no flag makes playback \
            take over a pane that already has a shell in it. The default is `.`, the \
            focused pane.\n\n\
            The pane is resized to the recording's own grid first, and to each resize the \
            recording contains, so lines wrap where they wrapped when it was captured; \
            --no-fit leaves the grid alone. When the recording ends the pane holds its \
            final frame until you kill it, so nothing races the last byte; --close ends \
            the pane instead.\n\n\
            Examples:\n  \
            phux play demo.cast\n  \
            phux play demo.cast work:1.0 --speed 2\n  \
            phux play demo.cast --loop --idle-limit 0.5 --json"
    )]
    #[usage(help_heading = "More", display_order = 61)]
    Play {
        /// The .cast file to play.
        #[usage(value_name = "FILE")]
        file: std::path::PathBuf,

        /// Selector for the pane the playback pane is created beside.
        /// Defaults to `.` (the focused pane). Never written to.
        #[usage(value_name = "TARGET")]
        target: Option<String>,

        /// Playback rate. 1 is real time, 2 is twice as fast, 0.5 half
        /// speed. Between 0.01 and 100; no events are ever dropped.
        #[usage(long, value_name = "N", default = "1")]
        speed: play::SpeedArg,

        /// Collapse any pause longer than SECS down to SECS. Defaults to
        /// the idle limit the recording itself declares; 0 plays the raw
        /// timeline.
        #[usage(long = "idle-limit", value_name = "SECS")]
        idle_limit: Option<f64>,

        /// Repeat the recording. Bare `--loop` repeats until the pane is
        /// killed; `--loop N` plays it N times.
        #[usage(long = "loop", value_name = "N", num_args = 0..=1,
              default_missing = "0")]
        loops: Option<u32>,

        /// Placement axis: `horizontal` puts the new pane to the right;
        /// `vertical` puts it below.
        #[usage(long, value_enum, default = "horizontal")]
        split: SpawnSplit,

        /// Fraction of the split retained by TARGET.
        #[usage(
            long,
            default = "0.5",
            default_value_t = 0.5,
            validate = "float(value) > 0 && float(value) < 1",
            validate_error = "ratio must be finite and strictly between 0 and 1"
        )]
        ratio: f32,

        /// Leave the pane's grid alone instead of fitting it to the
        /// recording's. Output wider than the pane will wrap.
        #[usage(long = "no-fit")]
        no_fit: bool,

        /// Close the pane when playback ends, instead of holding the final
        /// frame until it is killed.
        #[usage(long)]
        close: bool,

        #[usage(flatten)]
        json: JsonOpt,

        /// Internal: this process IS the pane, so write the recording to
        /// stdout rather than spawning one. Hidden because it is an
        /// implementation detail of the pane this verb creates, not a
        /// promise that phux ships a shell-level cast viewer.
        #[usage(long = "pty-writer", hide)]
        pty_writer: bool,
    },

    #[usage(help_heading = "Agents", display_order = 32)]
    Ask(AskArgs),

    #[usage(help_heading = "Agents", display_order = 30)]
    Agent(AgentArgs),

    #[usage(help_heading = "Panes", display_order = 24)]
    Run(RunArgs),

    #[usage(help_heading = "Maintain", display_order = 54)]
    Config(ConfigArgs),

    #[usage(help_heading = "Maintain", display_order = 55)]
    Plugin(PluginArgs),

    #[usage(help_heading = "Maintain", display_order = 59)]
    Theme(ThemeArgs),

    #[usage(help_heading = "Maintain", display_order = 60)]
    Omarchy(OmarchyArgs),

    #[usage(help_heading = "More", display_order = 65)]
    Archive(ArchiveArgs),

    #[usage(help_heading = "Panes", display_order = 28)]
    Tag(TagArgs),

    /// Bridge stdin/stdout to the local server socket for SSH-stdio transport.
    ///
    /// The remote end of the SSH-stdio transport: `ssh HOST phux
    /// stdio-bridge` gives the dialing side a byte-transparent pipe to the
    /// phux server's Unix socket on HOST — the federation hub dials
    /// `ssh://` satellites through it. The bridge neither
    /// parses nor injects bytes; stdout is protocol-only and diagnostics
    /// go to stderr. Exits when either side closes.
    // Hidden: machine-only plumbing invoked as `ssh HOST phux stdio-bridge`.
    #[usage(name = "stdio-bridge", hide)]
    StdioBridge {},

    // Hidden: `phux attach --ssh` runs it over ssh.
    #[usage(name = "bootstrap", hide)]
    Bootstrap(BootstrapArgs),

    #[usage(help_heading = "Machines", display_order = 44)]
    Relay(RelayArgs),

    #[usage(help_heading = "Machines", display_order = 43)]
    Pair(PairArgs),

    #[usage(help_heading = "Machines", display_order = 45)]
    Workload(WorkloadArgs),

    #[usage(alias = "machine", help_heading = "Machines", display_order = 40)]
    Host(HostArgs),

    #[usage(help_heading = "Machines", display_order = 42)]
    Service(ServiceArgs),
    #[usage(help_heading = "More", display_order = 70)]
    Completion(CompletionArgs),

    #[usage(help_heading = "More", display_order = 68)]
    Mcp(McpArgs),

    #[usage(help_heading = "More", display_order = 69)]
    Skill(SkillArgs),

    /// Capture an idea
    ///
    /// TEXT is written to the local ideas spool first, then signed and posted
    /// to `https://ingest.phux.sh/ideas/capture`. Success prints the capture
    /// id. A network failure leaves the idea queued; the next `phux idea`
    /// sends it with the same idempotency key and a fresh signature.
    /// `--flush` sends queued ideas and does not capture a new one.
    ///
    /// `--private` ensures the body contains the `#private` tag.
    ///
    /// `PHUX_IDEAS_KEY_ID` and `PHUX_IDEAS_KEY` are read from
    /// `$XDG_CONFIG_HOME/phux/ops.env` (`~/.config/phux/ops.env` when
    /// `XDG_CONFIG_HOME` is unset). `PHUX_IDEAS_KEY_ID` is the key id, for
    /// example `cli-mbp`. `PHUX_IDEAS_KEY` is 64 hex characters: the 32-byte
    /// HMAC secret. The secret is never printed.
    ///
    /// Examples:
    ///   phux idea "ship the spool before the post"
    ///   phux idea --private "keep this off the triage model"
    ///   phux idea --flush
    #[usage(help_heading = "More", display_order = 67)]
    Idea {
        /// Ensure the body contains the `#private` tag.
        #[usage(long)]
        private: bool,

        /// Send queued ideas and do not capture a new one.
        #[usage(long)]
        flush: bool,

        /// Idea text. Required unless `--flush`.
        #[usage(value_name = "TEXT")]
        text: Option<String>,
    },

    #[usage(help_heading = "Maintain", display_order = 51)]
    Doctor(DoctorArgs),

    #[usage(alias = "p")]
    #[usage(help_heading = "Sessions", display_order = 16)]
    Project(ProjectArgs),

    #[usage(help_heading = "More", display_order = 66)]
    Worktree(WorktreeArgs),

    #[usage(help_heading = "Maintain", display_order = 52)]
    Logs(LogsArgs),

    #[usage(alias = "bug")]
    #[usage(help_heading = "Maintain", display_order = 53)]
    Report(ReportArgs),

    #[usage(name = "gen-reference-docs", hide)]
    GenReferenceDocs(GenReferenceDocsArgs),
}
