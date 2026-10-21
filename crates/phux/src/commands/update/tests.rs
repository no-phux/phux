#![allow(clippy::unwrap_used, reason = "tests")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use phux_client_runtime::release::{self as shared, Asset, Build, Channel, Product, Version};

use super::apply::BACKUP_DIR;
use super::channel::{self, ChannelArg};
use super::release::{ReleaseSource, current_version, host_target};
use super::signature::tests::TestKey;
use super::source::{Install, InstallSource};
use super::{
    Action, Handoff, Outcome, Refusal, UpdateEnv, UpdateError, UpdateOpts, execute, plan,
    reconcile_installed,
};

const NEXT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn installed_updates_reconcile_in_human_and_json_modes() {
    let mut calls = Vec::new();
    reconcile_installed(Action::Installed, false, |print| calls.push(print));
    reconcile_installed(Action::Installed, true, |print| calls.push(print));
    reconcile_installed(Action::Checked, false, |print| calls.push(print));

    assert_eq!(calls, [true, false]);
}

/// The triple these tests publish artifacts under, resolved exactly as
/// the code under test resolves it (a hardcoded triple 404s elsewhere).
fn target() -> &'static str {
    host_target().expect("tests run on a platform with a published artifact")
}

const UNUSED_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A channel manifest naming `version` and `sha` with `digest` for this
/// host's tarball.
fn manifest(channel: Channel, version: &str, sha: &str, digest: &str) -> String {
    serde_json::json!({
        "schema_version": 1,
        "channel": channel.as_str(),
        "generated_at": "2026-01-01T00:00:00Z",
        "products": {"phux": {
            "version": version,
            "sha": sha,
            "protocol": "0.9",
            "artifacts": {target(): {"sha256": digest}},
        }},
    })
    .to_string()
}

/// A [`ReleaseSource`] backed by a manifest per channel and a map of URL
/// to bytes; no network.
#[derive(Debug)]
struct FakeReleases {
    manifests: HashMap<&'static str, String>,
    files: HashMap<String, Vec<u8>>,
    downloads: RefCell<Vec<String>>,
}

impl FakeReleases {
    /// A stable manifest naming `latest`, whose artifact is not served.
    fn new(latest: &str) -> Self {
        let mut fake = Self {
            manifests: HashMap::new(),
            files: HashMap::new(),
            downloads: RefCell::new(Vec::new()),
        };
        let version = latest.trim_start_matches('v');
        fake.name(Channel::Stable, version, &"c".repeat(40), UNUSED_DIGEST);
        fake
    }

    fn name(&mut self, channel: Channel, version: &str, sha: &str, digest: &str) {
        self.manifests
            .insert(channel.as_str(), manifest(channel, version, sha, digest));
    }

    fn serve(&mut self, url: &str, bytes: Vec<u8>) {
        self.files.insert(url.to_owned(), bytes);
    }
}

impl ReleaseSource for FakeReleases {
    fn manifest(&self, channel: Channel) -> Result<String, UpdateError> {
        self.manifests
            .get(channel.as_str())
            .cloned()
            .ok_or_else(|| UpdateError::Fetch(format!("no {} manifest", channel.as_str())))
    }

    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError> {
        self.downloads.borrow_mut().push(url.to_owned());
        let bytes = self
            .files
            .get(url)
            .ok_or_else(|| UpdateError::Fetch(format!("404 for {url}")))?;
        fs::write(dest, bytes)
            .map_err(|err| UpdateError::Fetch(format!("could not write {}: {err}", dest.display())))
    }
}

fn opts() -> UpdateOpts {
    UpdateOpts {
        check: false,
        dry_run: false,
        tag: None,
        channel: None,
        rollback: false,
        no_restart: true,
        close_agent_sessions: false,
        json: false,
    }
}

fn check() -> UpdateOpts {
    UpdateOpts {
        check: true,
        ..opts()
    }
}

fn install_at(path: &Path, source: InstallSource) -> Install {
    Install {
        source,
        executable: path.to_owned(),
        nixos: false,
        unknown_reason: None,
    }
}

fn direct(path: &Path) -> Install {
    install_at(path, InstallSource::DirectRelease)
}

/// Run `execute` against `fake` with a fixed handoff answer.
fn run(
    fake: &FakeReleases,
    install: Install,
    handoff: Handoff,
    opts: &UpdateOpts,
) -> Result<Outcome, UpdateError> {
    let handoff = move || handoff.clone();
    let release_key = TestKey::new(RELEASE_SEED).public();
    let env = UpdateEnv {
        releases: fake,
        handoff: &handoff,
        install,
        release_keys: &[&release_key],
    };
    execute(opts, &env)
}

/// The seed of the key fake releases are signed with and `run` trusts.
const RELEASE_SEED: u8 = 7;

/// Sign `sidecar` with `key` and serve the signature beside it.
fn sign(fake: &mut FakeReleases, artifact: &Asset, sidecar: &str, key: &TestKey) {
    fake.serve(&artifact.sidecar_url(), sidecar.as_bytes().to_vec());
    fake.serve(
        &artifact.signature_url(),
        key.sign(sidecar.as_bytes()).into_bytes(),
    );
}

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// Build a release tarball plus its signed sidecar, serve them where the real
/// naming puts them, and name it in the stable manifest.
fn publish(fake: &mut FakeReleases, workdir: &Path, tag: &str) -> Asset {
    let version = tag.trim_start_matches('v');
    let (artifact, digest) =
        publish_artifact(fake, workdir, &Build::Stable(self::version(version)), tag);
    fake.name(Channel::Stable, version, &"c".repeat(40), &digest);
    artifact
}

fn publish_next(fake: &mut FakeReleases, workdir: &Path, sha: &str) -> Asset {
    let (artifact, digest) = publish_artifact(fake, workdir, &Build::Next(sha.to_owned()), sha);
    fake.name(Channel::Next, "9.9.9", sha, &digest);
    artifact
}

/// A packed release served with its sidecar; the manifest is untouched.
fn publish_artifact(
    fake: &mut FakeReleases,
    workdir: &Path,
    build: &Build,
    label: &str,
) -> (Asset, String) {
    let artifact = shared::asset(Product::Phux, build, target());
    let build = workdir.join("build").join(&artifact.stem);
    fs::create_dir_all(&build).unwrap();
    for name in ["phux", "phux-mcp"] {
        fs::write(build.join(name), format!("{name} {label}")).unwrap();
        fs::set_permissions(build.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    for name in ["README.md", "LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.md"] {
        fs::write(build.join(name), b"text").unwrap();
    }

    let archive = workdir.join(&artifact.name);
    let status = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(workdir.join("build"))
        .arg(&artifact.stem)
        .status()
        .unwrap();
    assert!(status.success());

    let digest = super::apply::sha256_file(&archive).unwrap();
    fake.serve(&artifact.url, fs::read(&archive).unwrap());
    let sidecar = format!("{digest}  {}\n", artifact.name);
    sign(fake, &artifact, &sidecar, &TestKey::new(RELEASE_SEED));
    (artifact, digest)
}

/// The plan for an install of `installed` against a manifest naming
/// `version` and `sha`, optionally pinned.
fn plan_for(
    installed: &Build,
    channel: Channel,
    version: &str,
    sha: &str,
    pin: Option<&str>,
) -> Result<super::Plan, UpdateError> {
    let body = manifest(channel, version, sha, UNUSED_DIGEST);
    let published = super::release::published(&body, channel, target());
    let pin = pin.map(super::release::validate_tag).transpose()?;
    let current = match installed {
        Build::Stable(version) => *version,
        Build::Next(_) => self::version("0.13.0"),
    };
    plan(
        direct(Path::new("/home/ada/.local/bin/phux")),
        current,
        installed,
        channel,
        published,
        pin,
        target(),
    )
}

/// A bin directory holding a "current" install.
fn seed_bin(scratch: &Path) -> PathBuf {
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for name in ["phux", "phux-mcp"] {
        fs::write(bin.join(name), format!("old {name}")).unwrap();
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

#[test]
fn a_stable_plan_classifies_updates_downgrades_and_bad_tags() {
    let installed = Build::Stable(version("0.12.1"));
    let plan = |pin| plan_for(&installed, Channel::Stable, "0.13.0", &"c".repeat(40), pin);
    let newer = plan(None).unwrap();
    assert!(newer.update_available && newer.changes_version);
    assert_eq!(newer.target.tag(), "v0.13.0");
    assert_eq!(
        newer.vouched().map(|entry| entry.sha256.as_str()),
        Some(UNUSED_DIGEST)
    );

    // An explicit older tag is a change, not an update, and the manifest
    // does not vouch for it.
    let older = plan(Some("v0.11.0")).unwrap();
    assert!(!older.update_available && older.changes_version);
    assert_eq!(older.target.tag(), "v0.11.0");
    assert!(older.vouched().is_none());

    let err = plan(Some("nightly")).unwrap_err();
    assert!(matches!(err, UpdateError::InvalidTag(_)), "{err:?}");
}

#[test]
fn refusals_cover_every_non_mutable_source_and_carry_a_command() {
    for (source, expected) in [
        (InstallSource::Nix, Refusal::ImmutableStore),
        (InstallSource::Homebrew, Refusal::PackageManaged),
        (InstallSource::Cargo, Refusal::PackageManaged),
        (InstallSource::Unknown, Refusal::UnknownSource),
    ] {
        let refusal = Refusal::of(source).unwrap();
        assert_eq!(refusal, expected);
        let install = install_at(Path::new("/somewhere/phux"), source);
        assert!(!refusal.message(&install).is_empty());
        assert!(!Refusal::remedy(&install).is_empty());
    }
    assert!(Refusal::of(InstallSource::DirectRelease).is_none());
}

/// `--check` downloads nothing, names both rail switches for an install
/// phux owns, and gives a package-managed install its native command
/// instead of a switch.
#[test]
fn check_reports_without_downloading_anything() {
    let fake = FakeReleases::new("v0.13.0");
    let outcome = run(
        &fake,
        direct(Path::new("/home/ada/.local/bin/phux")),
        Handoff::Skipped,
        &check(),
    )
    .unwrap();
    assert_eq!(outcome.action, Action::Checked);
    assert_eq!(outcome.plan.latest_tag(), "v0.13.0");
    assert!(outcome.artifact.is_none());
    assert!(
        fake.downloads.borrow().is_empty(),
        "--check must not download"
    );

    let doc = outcome.document();
    assert_eq!(doc["schema_version"], 2);
    assert_eq!(doc["action"], "checked");
    assert_eq!(doc["install"]["source"], "direct-release");
    assert_eq!(doc["install"]["mutable"], true);
    assert_eq!(doc["latest_version"], "v0.13.0");
    assert_eq!(doc["artifact"], serde_json::Value::Null);
    assert_eq!(doc["server_handoff"], serde_json::Value::Null);

    let switch = outcome
        .lines()
        .into_iter()
        .find(|line| line.starts_with("switch:"))
        .expect("the check report must name the rail switches");
    assert!(switch.contains("phux channel next"), "{switch}");
    assert!(switch.contains("phux channel latest"), "{switch}");

    let brew = run(
        &fake,
        install_at(
            Path::new("/opt/homebrew/Cellar/phux/0.12.1/bin/phux"),
            InstallSource::Homebrew,
        ),
        Handoff::Skipped,
        &check(),
    )
    .unwrap();
    let doc = brew.document();
    assert_eq!(doc["install"]["mutable"], false);
    assert_eq!(
        doc["install"]["native_command"],
        "brew upgrade no-phux/tap/phux"
    );
    let lines = brew.lines();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("brew upgrade no-phux/tap/phux")),
        "{lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.starts_with("switch:")),
        "a refused install must not be told to switch: {lines:?}"
    );
}

#[test]
fn dry_run_downloads_and_verifies_but_installs_nothing() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = seed_bin(scratch.path());
    let mut fake = FakeReleases::new("v9.9.9");
    publish(&mut fake, scratch.path(), "v9.9.9");

    let outcome = run(
        &fake,
        direct(&bin.join("phux")),
        Handoff::Skipped,
        &UpdateOpts {
            dry_run: true,
            ..opts()
        },
    )
    .unwrap();

    assert_eq!(outcome.action, Action::Planned);
    assert!(outcome.digest.is_some(), "the checksum must be verified");
    assert_eq!(outcome.binaries, vec!["phux-mcp", "phux"]);
    assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
    assert!(!bin.join(BACKUP_DIR).exists());
    let leftovers: Vec<_> = fs::read_dir(&bin)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".phux-update-"))
        .collect();
    assert!(leftovers.is_empty(), "staging leaked: {leftovers:?}");
}

#[test]
fn a_full_update_installs_verifies_and_hands_off() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = seed_bin(scratch.path());
    let mut fake = FakeReleases::new("v9.9.9");
    let artifact = publish(&mut fake, scratch.path(), "v9.9.9");

    let outcome = run(
        &fake,
        direct(&bin.join("phux")),
        Handoff::Upgrading,
        &UpdateOpts {
            no_restart: false,
            ..opts()
        },
    )
    .unwrap();

    assert_eq!(outcome.action, Action::Installed);
    assert_eq!(fs::read(bin.join("phux")).unwrap(), b"phux v9.9.9");
    assert_eq!(fs::read(bin.join("phux-mcp")).unwrap(), b"phux-mcp v9.9.9");
    assert_eq!(outcome.handoff, Some(Handoff::Upgrading));

    let doc = outcome.document();
    assert_eq!(doc["action"], "installed");
    assert_eq!(doc["target_version"], "v9.9.9");
    assert_eq!(doc["artifact"]["archive"], artifact.name);
    assert_eq!(doc["server_handoff"]["result"], "upgrading");
    assert!(doc["artifact"]["sha256"].is_string());
    assert!(doc["backup"].is_string());
    assert_eq!(doc["protocol"]["target"], "0.9");
    assert_eq!(doc["protocol"]["current"], super::current_protocol());

    assert_eq!(doc["artifact"]["signature_url"], artifact.signature_url());

    // The signed sidecar is the gate, cross-checked against the manifest.
    let fetched = fake.downloads.borrow().clone();
    assert_eq!(
        fetched,
        vec![
            artifact.url.clone(),
            artifact.sidecar_url(),
            artifact.signature_url()
        ]
    );
}

mod releases;
