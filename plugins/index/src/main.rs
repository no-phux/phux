//! CLI for the static plugin index.
//!
//! `--offline` reads fixtures and does not use `GITHUB_TOKEN`. Without it,
//! the public GitHub API is queried; `GITHUB_TOKEN` is sent only to raise
//! the rate limit.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use phux_plugin_index::{
    Options, Source, build_index, credential_from, render_json, render_markdown,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("phux-plugin-index: {failure}");
            if failure.usage {
                ExitCode::from(2)
            } else {
                ExitCode::from(1)
            }
        }
    }
}

struct Failure {
    message: String,
    usage: bool,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Failure {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            usage: true,
        }
    }

    fn fail(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            usage: false,
        }
    }
}

fn run() -> Result<(), Failure> {
    let args = Args::parse()?;
    let examples = args
        .examples
        .unwrap_or_else(|| args.repo_root.join("examples/plugins"));
    let generated_at = args.generated_at.unwrap_or_else(utc_now);
    let token = credential_from(args.offline.is_some(), args.token.as_deref())
        .map_err(|err| Failure::fail(err.to_string()))?;
    let source = match args.offline {
        Some(fixtures) => Source::Offline { fixtures },
        None => Source::github(token),
    };
    let index = build_index(&Options {
        source,
        examples_dir: examples,
        examples_repo: args.examples_repo,
        repo_root: args.repo_root,
        generated_at,
    })
    .map_err(|err| Failure::fail(err.to_string()))?;
    let json = render_json(&index).map_err(|err| Failure::fail(err.to_string()))?;
    let markdown = render_markdown(&index);
    fs::create_dir_all(&args.out).map_err(|err| Failure::fail(err.to_string()))?;
    let json_path = args.out.join("plugin-index.json");
    let markdown_path = args.out.join("plugin-index.md");
    fs::write(&json_path, json).map_err(|err| Failure::fail(err.to_string()))?;
    fs::write(&markdown_path, markdown).map_err(|err| Failure::fail(err.to_string()))?;
    println!(
        "wrote {} ({} plugins, {} skipped)",
        json_path.display(),
        index.plugins.len(),
        index.skipped.len()
    );
    Ok(())
}

struct Args {
    out: PathBuf,
    offline: Option<PathBuf>,
    examples: Option<PathBuf>,
    examples_repo: String,
    repo_root: PathBuf,
    generated_at: Option<String>,
    token: Option<String>,
}

impl Args {
    fn parse() -> Result<Self, Failure> {
        let mut out = None;
        let mut offline = None;
        let mut examples = None;
        let mut examples_repo = None;
        let mut repo_root = None;
        let mut generated_at = None;
        let mut tokens = env::args().skip(1);
        while let Some(arg) = tokens.next() {
            match arg.as_str() {
                "--help" | "-h" => {
                    print!("{HELP}");
                    std::process::exit(0);
                }
                "--out" => out = Some(need(&mut tokens, "--out")?),
                "--offline" => offline = Some(need(&mut tokens, "--offline")?),
                "--examples" => examples = Some(need(&mut tokens, "--examples")?),
                "--examples-repo" => examples_repo = Some(need(&mut tokens, "--examples-repo")?),
                "--repo-root" => repo_root = Some(need(&mut tokens, "--repo-root")?),
                "--generated-at" => generated_at = Some(need(&mut tokens, "--generated-at")?),
                other => {
                    return Err(Failure::usage(format!("unknown argument {other}\n{HELP}")));
                }
            }
        }
        let Some(out) = out else {
            return Err(Failure::usage(format!("--out is required\n{HELP}")));
        };
        // Read the token only for a networked build, so an offline run cannot
        // send it and a test environment cannot leak it into fixtures.
        let token = if offline.is_some() {
            None
        } else {
            env::var("GITHUB_TOKEN").ok()
        };
        Ok(Self {
            out: PathBuf::from(out),
            offline: offline.map(PathBuf::from),
            examples: examples.map(PathBuf::from),
            examples_repo: examples_repo.unwrap_or_else(|| "no-phux/phux".to_owned()),
            repo_root: repo_root.map_or_else(|| PathBuf::from("."), PathBuf::from),
            generated_at,
            token,
        })
    }
}

fn need(tokens: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, Failure> {
    tokens
        .next()
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| Failure::usage(format!("{flag} needs a value")))
}

fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format_unix(secs)
}

fn format_unix(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let tod = secs % 86_400;
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;
    let second = tod % 60;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

const HELP: &str = "\
Build a static phux plugin index from the public GitHub topic phux-plugin
and the example plugins in this checkout.

Plugins run as the user and aren't sandboxed (see docs/plugins.md).

Usage:
  phux-plugin-index --out DIR [options]

Options:
  --out DIR            Write plugin-index.json and plugin-index.md here
  --offline DIR        Read fixtures from DIR instead of GitHub
  --examples PATH      Example plugin parent (default: <repo-root>/examples/plugins)
  --examples-repo NAME owner/repo for those examples (default: no-phux/phux)
  --repo-root PATH     Checkout root for example subdirectories (default: .)
  --generated-at STAMP UTC timestamp YYYY-MM-DDTHH:MM:SSZ (default: now)

GITHUB_TOKEN, when set and --offline is not, is sent only as a bearer
token so GitHub's rate limit is higher. The public API works without it.
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_epoch_formats_as_utc() {
        assert_eq!(format_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
