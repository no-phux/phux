use super::Cli;
use crate::commands::Command;

mod aliases;
mod help;
mod scope;
mod verbs;

fn argv(words: &[&str]) -> Vec<std::ffi::OsString> {
    words.iter().map(std::ffi::OsString::from).collect()
}
