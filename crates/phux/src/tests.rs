use super::Cli;
use crate::commands::Command;

mod aliases;
mod help;
mod scope;
mod verbs;

fn argv(words: &[&str]) -> Vec<std::ffi::OsString> {
    words.iter().map(std::ffi::OsString::from).collect()
}

/// `argv` of a whitespace-separated command line.
fn words(line: &str) -> Vec<std::ffi::OsString> {
    line.split_whitespace()
        .map(std::ffi::OsString::from)
        .collect()
}

/// Parse a whitespace-separated command line.
fn parse(line: &str) -> Result<Cli, String> {
    crate::parse_cli(line.split_whitespace())
}

/// Each line parses, or is a usage error, as its flag says.
fn assert_parses(cases: &[(&str, bool)]) {
    for &(line, ok) in cases {
        assert_eq!(parse(line).is_ok(), ok, "{line}: {:?}", parse(line).err());
    }
}
