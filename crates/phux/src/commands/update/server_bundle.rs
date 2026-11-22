//! Install the macOS `phux Server.app` a stable release publishes beside
//! the CLI (`phux-server-<release>-<target>.tar.gz`) into the stable layout
//! `phux_config::server_bundle` reads: `Versions/<release>/phux Server.app`
//! plus `Versions/Current -> <release>`, keeping the previous release for
//! rollback.
//! The same gate as the CLI applies: a release key signed the digest and
//! the bytes match it.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use phux_client_runtime::release::Asset;
use phux_config::server_bundle::APP_NAME;

use super::apply::{self, Staging};
use super::{ReleaseSource, UpdateError};

/// The server bundle asset beside the CLI asset `cli`.
pub(crate) fn asset_beside(cli: &Asset) -> Asset {
    let rename = |name: &str| name.replacen("phux-", "phux-server-", 1);
    let url = cli.url.rsplit_once('/').map_or_else(
        || rename(&cli.url),
        |(base, _)| format!("{base}/{}", rename(&cli.name)),
    );
    Asset {
        stem: rename(&cli.stem),
        name: rename(&cli.name),
        url,
    }
}

/// Download, verify and install the bundle for `release` (`vX.Y.Z`) under
/// `root`.
pub(super) fn install(
    releases: &dyn ReleaseSource,
    release_keys: &[&str],
    cli: &Asset,
    release: &str,
    root: &Path,
) -> Result<(), UpdateError> {
    let asset = asset_beside(cli);
    fs::create_dir_all(root).map_err(|err| {
        UpdateError::Install(format!("could not create {}: {err}", root.display()))
    })?;
    // Under the root, so the final rename stays on one filesystem.
    let staging = Staging::create(root)?;
    let archive = staging.path().join(&asset.name);
    releases.download(&asset.url, &archive)?;
    let expected = super::signed_digest(releases, release_keys, &asset, staging.path())?;
    apply::verify_digest(&archive, &expected, &asset.name)?;
    let listing = apply::tar(&["-tzf", &archive.to_string_lossy()])?;
    let prefix = format!("{}/{APP_NAME}/", asset.stem);
    for member in listing.lines().map(str::trim).filter(|m| !m.is_empty()) {
        let inside = member == asset.stem
            || member == format!("{}/", asset.stem)
            || member
                .strip_prefix(&prefix)
                .is_some_and(|rest| !rest.split('/').any(|part| part == ".." || part == "."));
        if !inside {
            return Err(UpdateError::Archive(format!(
                "unexpected member `{member}` in {}",
                asset.name
            )));
        }
    }
    apply::tar(&[
        "-xzf",
        &archive.to_string_lossy(),
        "-C",
        &staging.path().to_string_lossy(),
    ])?;
    let app = staging.path().join(&asset.stem).join(APP_NAME);
    refuse_links(&app)?;
    if !app.join("Contents/MacOS/phux").is_file() {
        return Err(UpdateError::Archive(format!(
            "{} has no Contents/MacOS/phux",
            asset.name
        )));
    }
    publish(root, &app, release)
        .map_err(|err| UpdateError::Install(format!("could not install {APP_NAME}: {err}")))
}

/// A bundle from a release archive holds only directories and regular files.
fn refuse_links(dir: &Path) -> Result<(), UpdateError> {
    let entries = fs::read_dir(dir)
        .map_err(|err| UpdateError::Archive(format!("{} is unreadable: {err}", dir.display())))?;
    for entry in entries {
        let path = entry
            .map_err(|err| UpdateError::Archive(format!("unpack directory error: {err}")))?
            .path();
        let kind = fs::symlink_metadata(&path)
            .map_err(|err| {
                UpdateError::Archive(format!("{} is unreadable: {err}", path.display()))
            })?
            .file_type();
        if kind.is_dir() {
            refuse_links(&path)?;
        } else if !kind.is_file() {
            return Err(UpdateError::Archive(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Move `app` to `root/Versions/<release>/phux Server.app`, point `Current`
/// at it, and prune every other version but the one `Current` named before.
/// A release already present is kept as it is: a running server may be
/// executing it.
pub(crate) fn publish(root: &Path, app: &Path, release: &str) -> std::io::Result<()> {
    let version = root.join("Versions").join(release);
    if !version.join(APP_NAME).exists() {
        fs::create_dir_all(&version)?;
        fs::rename(app, version.join(APP_NAME))?;
    }
    let previous = point_current(root, release)?;
    let mut keep = vec![
        OsStr::new("Current").to_owned(),
        OsStr::new(release).to_owned(),
    ];
    keep.extend(previous);
    prune(root, &keep);
    Ok(())
}

/// Atomically point `root/Versions/Current` at `release`, returning what
/// it named before.
pub(crate) fn point_current(
    root: &Path,
    release: &str,
) -> std::io::Result<Option<std::ffi::OsString>> {
    let current = root.join("Versions/Current");
    let previous = fs::read_link(&current).ok().map(PathBuf::into_os_string);
    let next = root.join("Versions/.Current.next");
    let _ = fs::remove_file(&next);
    std::os::unix::fs::symlink(release, &next)?;
    // rename(2) replaces the link itself, never what it points at.
    fs::rename(&next, &current)?;
    Ok(previous)
}

/// Remove every entry of `root/Versions` not named in `keep` (which names
/// `Current` itself). Best effort:
/// what cannot be removed now is removed by the next update.
pub(crate) fn prune(root: &Path, keep: &[std::ffi::OsString]) {
    let Ok(entries) = fs::read_dir(root.join("Versions")) else {
        return;
    };
    for entry in entries.flatten() {
        if !keep.iter().any(|name| *name == entry.file_name()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_in(dir: &Path, marker: &str) -> PathBuf {
        let app = dir.join(APP_NAME);
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/phux"), marker).unwrap();
        app
    }

    #[test]
    fn publishing_flips_current_and_keeps_one_previous_version() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Server");
        for release in ["v1.0.0", "v1.1.0", "v1.2.0"] {
            let incoming = tmp.path().join(format!("in-{release}"));
            publish(&root, &app_in(&incoming, release), release).unwrap();
            assert_eq!(
                fs::read_link(root.join("Versions/Current")).unwrap(),
                Path::new(release)
            );
        }
        let mut versions: Vec<_> = fs::read_dir(root.join("Versions"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        versions.sort();
        assert_eq!(versions, ["Current", "v1.1.0", "v1.2.0"]);
        let exe = phux_config::server_bundle::current_executable(&root);
        assert_eq!(fs::read_to_string(exe).unwrap(), "v1.2.0");

        // Rolling back re-points Current; nothing is pruned that is kept.
        point_current(&root, "v1.1.0").unwrap();
        let exe = phux_config::server_bundle::current_executable(&root);
        assert_eq!(fs::read_to_string(exe).unwrap(), "v1.1.0");
    }

    #[test]
    fn the_server_asset_sits_beside_the_cli_asset() {
        let cli = Asset {
            stem: "phux-v1.2.3-aarch64-apple-darwin".to_owned(),
            name: "phux-v1.2.3-aarch64-apple-darwin.tar.gz".to_owned(),
            url: "https://github.com/no-phux/phux/releases/download/v1.2.3/\
                  phux-v1.2.3-aarch64-apple-darwin.tar.gz"
                .to_owned(),
        };
        let server = asset_beside(&cli);
        assert_eq!(server.stem, "phux-server-v1.2.3-aarch64-apple-darwin");
        assert_eq!(
            server.name,
            "phux-server-v1.2.3-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            server.url,
            "https://github.com/no-phux/phux/releases/download/v1.2.3/\
             phux-server-v1.2.3-aarch64-apple-darwin.tar.gz"
        );
    }
}
