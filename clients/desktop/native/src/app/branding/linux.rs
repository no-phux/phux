//! User-local launcher override; signed/release bundle files remain immutable.
use super::Profile;
use anyhow::{Context as _, Result};
#[cfg(all(target_os = "linux", not(test)))]
use std::path::PathBuf;
use std::{
    fs,
    io::Write as _,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(all(target_os = "linux", not(test)))]
pub(super) fn apply(profile: &Profile) -> Result<()> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .context("No user data directory for the launcher icon")?;
    let executable = std::env::current_exe()?;
    let entry = home.join("applications/dev.phux.desktop.desktop");
    let original = if entry.exists() {
        Some(fs::read_to_string(&entry).context("Could not read the existing launcher entry")?)
    } else {
        let shipped = executable
            .parent()
            .and_then(Path::parent)
            .map(|root| root.join("share/applications/dev.phux.desktop.desktop"));
        shipped
            .filter(|path| path.exists())
            .map(fs::read_to_string)
            .transpose()?
    };
    install(profile, &home, &executable, original.as_deref())
}

fn escaped(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn executable_argument(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .context("The launcher executable path must be UTF-8")?;
    anyhow::ensure!(
        !value.contains('='),
        "The executable path cannot contain '=' in a desktop entry"
    );
    // Desktop Entry string escapes are decoded before Exec quoting/field codes.
    let quoted = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
        .replace('%', "%%");
    Ok(format!("\"{}\"", escaped(&quoted)))
}

fn install(
    profile: &Profile,
    home: &Path,
    executable: &Path,
    original: Option<&str>,
) -> Result<()> {
    let icon = home
        .join("phux-desktop/icons")
        .join(format!("{}.png", profile.id));
    let icon_value = escaped(
        icon.to_str()
            .context("The launcher icon path must be UTF-8")?,
    );
    let fallback = if original.is_none() {
        Some(format!(
            "[Desktop Entry]\nType=Application\nName=Phux\nExec={}\nTerminal=false\nCategories=Development;TerminalEmulator;\nStartupWMClass=dev.phux.desktop\n",
            executable_argument(executable)?
        ))
    } else {
        None
    };
    let source = original
        .or(fallback.as_deref())
        .context("No launcher entry")?;
    anyhow::ensure!(
        source
            .lines()
            .filter(|line| *line == "[Desktop Entry]")
            .count()
            == 1,
        "Invalid existing desktop entry"
    );
    let mut result = String::new();
    let mut main = false;
    for line in source.lines() {
        if line.starts_with('[') {
            main = line == "[Desktop Entry]";
        }
        if main && (line.starts_with("Icon=") || line.starts_with("Icon[")) {
            continue;
        }
        result.push_str(line);
        result.push('\n');
        if line == "[Desktop Entry]" {
            result.push_str(&format!("Icon={icon_value}\n"));
        }
    }
    write(&icon, &profile.png()?)?;
    write(
        &home.join("applications/dev.phux.desktop.desktop"),
        result.as_bytes(),
    )
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(path.parent().context("No launcher directory")?)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staged = path.with_extension(format!("{}.{stamp}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&staged, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(staged);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launcher_switch_preserves_custom_commands_and_other_desktop_groups() {
        let home = super::super::tests::location();
        let source = "[Desktop Entry]\nName=My Phux\nIcon=old\nExec=custom-launch --keep\n[Desktop Action New]\nIcon=other\nExec=other\n";
        for profile in super::super::profiles() {
            install(
                profile,
                &home,
                Path::new("/opt/Phux/bin/phux-desktop"),
                Some(source),
            )
            .unwrap();
            let text =
                fs::read_to_string(home.join("applications/dev.phux.desktop.desktop")).unwrap();
            assert!(text.contains("Exec=custom-launch --keep"));
            assert!(text.contains("Name=My Phux"));
            assert!(text.contains("[Desktop Action New]\nIcon=other"));
            assert!(text.contains(&format!("/{}.png", profile.id)));
            assert_eq!(
                fs::read(home.join(format!("phux-desktop/icons/{}.png", profile.id))).unwrap(),
                profile.png().unwrap()
            );
        }
        fs::remove_dir_all(home).unwrap();
    }
}
