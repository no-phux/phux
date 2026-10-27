//! Which backdrop a window wears: off, a built-in, or a `.wgsl` file in the
//! user's shader folder.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::program::{Program, ShaderError};

/// A shader that ships with the app.
#[derive(Debug, PartialEq, Eq)]
pub struct Builtin {
    /// Stable preference value.
    pub id: &'static str,
    pub name: &'static str,
    /// One line for Settings.
    pub blurb: &'static str,
    pub source: &'static str,
}

pub const BUILTINS: &[Builtin] = &[
    Builtin {
        id: "silk",
        name: "Silk",
        blurb: "A slow mesh gradient in your theme's colors.",
        source: include_str!("library/silk.wgsl"),
    },
    Builtin {
        id: "aurora",
        name: "Aurora",
        blurb: "Curtains of light that flow faster while agents work.",
        source: include_str!("library/aurora.wgsl"),
    },
    Builtin {
        id: "nebula",
        name: "Nebula",
        blurb: "Drifting gas and stars that lean toward the pointer.",
        source: include_str!("library/nebula.wgsl"),
    },
    Builtin {
        id: "rain",
        name: "Rain",
        blurb: "Falling glyphs; blocked agents burn amber.",
        source: include_str!("library/rain.wgsl"),
    },
    Builtin {
        id: "grid",
        name: "Grid",
        blurb: "A neon floor that rolls with your fleet.",
        source: include_str!("library/grid.wgsl"),
    },
];

/// The template "New shader" writes.
pub const STARTER: &str = include_str!("library/starter.wgsl");

/// The preference value for no backdrop.
pub const OFF: &str = "off";
const FILE_PREFIX: &str = "file:";

/// A parsed backdrop preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Off,
    Builtin(&'static Builtin),
    /// A file name in [`folder`].
    File(String),
}

impl Choice {
    /// Read a preference value. Anything unrecognized is off.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        if let Some(name) = value.strip_prefix(FILE_PREFIX) {
            return if valid_file_name(name) {
                Self::File(name.to_owned())
            } else {
                Self::Off
            };
        }
        BUILTINS
            .iter()
            .find(|builtin| builtin.id == value)
            .map_or(Self::Off, Self::Builtin)
    }

    /// The stable preference value.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Off => OFF.to_owned(),
            Self::Builtin(builtin) => builtin.id.to_owned(),
            Self::File(name) => format!("{FILE_PREFIX}{name}"),
        }
    }

    /// What Settings and the palette call it.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Off => "Off".to_owned(),
            Self::Builtin(builtin) => builtin.name.to_owned(),
            Self::File(name) => name.trim_end_matches(".wgsl").to_owned(),
        }
    }

    /// The file a [`Choice::File`] reads.
    #[must_use]
    pub fn path(&self) -> Option<PathBuf> {
        match self {
            Self::File(name) => Some(folder().join(name)),
            _ => None,
        }
    }
}

/// A program and where it came from, for change detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub program: Result<Program, ShaderError>,
    /// A file's modification time when it was read.
    pub modified: Option<SystemTime>,
}

/// Compile a choice. `None` for off.
#[must_use]
pub fn load(choice: &Choice) -> Option<Loaded> {
    match choice {
        Choice::Off => None,
        Choice::Builtin(builtin) => Some(Loaded {
            program: Program::compile(builtin.name, builtin.source),
            modified: None,
        }),
        Choice::File(name) => Some(load_file(&folder().join(name), name)),
    }
}

fn load_file(path: &Path, name: &str) -> Loaded {
    let modified = modified(path);
    let program = std::fs::read(path)
        .map_err(|error| ShaderError {
            at: None,
            message: format!("could not read {}: {error}", path.display()),
        })
        .and_then(|bytes| {
            String::from_utf8(bytes).map_err(|_| ShaderError {
                at: None,
                message: format!("{name} is not UTF-8 text"),
            })
        })
        .and_then(|source| Program::compile(name, &source));
    Loaded { program, modified }
}

/// When `path` last changed, if it exists.
#[must_use]
pub fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// The user's shader folder: `shaders` in the desktop's config directory.
#[must_use]
pub fn folder() -> PathBuf {
    crate::updates::config_dir().join("shaders")
}

/// A bare `.wgsl` file name: no directories, not hidden.
#[must_use]
pub fn valid_file_name(name: &str) -> bool {
    name.len() > ".wgsl".len()
        && name.len() <= 128
        && name.ends_with(".wgsl")
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
}

/// The `.wgsl` files in `dir`, sorted. A missing folder lists nothing.
#[must_use]
pub fn files_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| valid_file_name(name))
        .collect();
    names.sort_unstable_by_key(|name| name.to_lowercase());
    names
}

/// Write the starter template under a fresh name in `dir` and return it.
pub fn create_starter(dir: &Path) -> std::io::Result<String> {
    std::fs::create_dir_all(dir)?;
    for index in 1.. {
        let name = if index == 1 {
            "my-backdrop.wgsl".to_owned()
        } else {
            format!("my-backdrop-{index}.wgsl")
        };
        let mut options = std::fs::OpenOptions::new();
        match options.write(true).create_new(true).open(dir.join(&name)) {
            Ok(mut file) => {
                std::io::Write::write_all(&mut file, STARTER.as_bytes())?;
                return Ok(name);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("an unbounded range yields until a name is free")
}

#[cfg(test)]
mod tests;
