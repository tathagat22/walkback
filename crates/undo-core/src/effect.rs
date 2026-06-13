//! An `Effect` is a single change an agent made to the world, paired with
//! enough information to reverse it. This is the heart of the whole system:
//! anything that can describe its own inverse — a file, a directory, a symlink,
//! a network call — fits into the same journal and the same one-button rollback.

use crate::meta::Meta;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A reversible (or at least auditable) side effect.
///
/// The `Path*` / `File` / `Symlink` / `Dir` variants are fully reversible.
/// `HttpMutation` and `Exec` are recorded for audit and carry the shape needed
/// to reverse them later, but are not auto-reversed yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    /// A path that did not exist when we captured it. Inverse: delete whatever
    /// is now there (file, symlink, or whole directory tree).
    PathCreate { path: PathBuf },

    /// A regular file whose prior contents + metadata were captured.
    /// Inverse: restore the contents and re-apply mode/mtime.
    File {
        path: PathBuf,
        prev_blob: String,
        #[serde(default)]
        meta: Meta,
    },

    /// A symlink that existed at capture time. Inverse: recreate it pointing at
    /// `target` (we snapshot the link itself, never the file it points to).
    Symlink { path: PathBuf, target: PathBuf },

    /// A directory that existed at capture time, plus the names of its immediate
    /// children. Inverse: ensure the directory exists with its mode, and prune
    /// any children the agent *added* that weren't here originally.
    Dir {
        path: PathBuf,
        #[serde(default)]
        mode: u32,
        entries: Vec<String>,
    },

    /// A network mutation (POST/PUT/PATCH/DELETE). The `compensator` is the
    /// request that reverses it (e.g. a DELETE to undo a POST). Recorded only.
    HttpMutation {
        method: String,
        url: String,
        compensator: Option<HttpCompensator>,
    },

    /// A shell command. Audit-only; arbitrary commands have no general inverse.
    Exec { command: String, cwd: PathBuf },
}

/// The request that reverses an `HttpMutation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpCompensator {
    pub method: String,
    pub url: String,
    pub body: Option<String>,
}

impl Effect {
    /// Can this effect be reversed automatically?
    pub fn reversible(&self) -> bool {
        matches!(
            self,
            Effect::PathCreate { .. }
                | Effect::File { .. }
                | Effect::Symlink { .. }
                | Effect::Dir { .. }
        )
    }

    /// The filesystem path this effect concerns, if any.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Effect::PathCreate { path }
            | Effect::File { path, .. }
            | Effect::Symlink { path, .. }
            | Effect::Dir { path, .. } => Some(path),
            _ => None,
        }
    }

    /// A short, human-readable, log-friendly description.
    pub fn describe(&self) -> String {
        match self {
            Effect::PathCreate { path } => format!("created  {}", path.display()),
            Effect::File { path, .. } => format!("captured {}", path.display()),
            Effect::Symlink { path, .. } => format!("symlink  {}", path.display()),
            Effect::Dir { path, .. } => format!("dir      {}", path.display()),
            Effect::HttpMutation { method, url, .. } => format!("{method:<8} {url}"),
            Effect::Exec { command, .. } => format!("ran      {command}"),
        }
    }
}
