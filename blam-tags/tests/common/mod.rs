//! Helpers shared by the integration tests.
//!
//! Each file under `tests/` is its own crate, so this module is compiled into
//! every suite that says `mod common;` and most of them use only part of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub mod synthetic;

/// The `definitions/` submodule at the workspace root.
pub fn definitions_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../definitions")
}

/// One game's definitions folder, e.g. `definitions("halo3_mcc")`.
pub fn definitions(game: &str) -> PathBuf {
    definitions_root().join(game)
}

/// An editing kit, located through a `BLAM_TEST_*` variable naming its
/// **root** (the folder holding `tags` and `data`), which is the engine's
/// convention for every kit-gated suite.
///
/// Kits are found through the environment only. Guessing install locations
/// made a suite's behaviour depend on which machine ran it, and the guesses
/// were never right on CI anyway.
#[derive(Debug, Clone)]
pub struct KitRoot {
    root: PathBuf,
}

impl KitRoot {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn tags(&self) -> PathBuf {
        self.root.join("tags")
    }

    pub fn data(&self) -> PathBuf {
        self.root.join("data")
    }
}

/// The kit named by `env`, or `None` after saying so on stderr.
///
/// A suite that finds no kit passes without testing anything, so the skip has
/// to be visible: `cargo test -- --nocapture` (or a failing run's output)
/// shows `skipped: no BLAM_TEST_H3EK` instead of an unexplained green. A
/// variable that is set but names something without a `tags` folder is a
/// misconfiguration rather than an absent kit, and fails the test.
pub fn kit(env: &str) -> Option<KitRoot> {
    let Some(value) = std::env::var_os(env) else {
        eprintln!("skipped: no {env}");
        return None;
    };
    let root = PathBuf::from(value);
    assert!(
        root.join("tags").is_dir(),
        "{env} is set to {} but that has no tags folder; it names the kit root",
        root.display()
    );
    Some(KitRoot { root })
}

/// Every file under `root` whose name ends in `.ext`, compared without regard
/// to case (`.JMS` and `.jms` both ship), sorted so a run is repeatable.
/// Matched on the name rather than `Path::extension`, which reads the two
/// kit files named exactly `.JMS` as having none. A missing `root` yields
/// nothing.
pub fn walk(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()).is_some_and(|name| {
                name.len() > ext.len()
                    && name.as_bytes()[name.len() - ext.len() - 1] == b'.'
                    && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext)
            }) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}
