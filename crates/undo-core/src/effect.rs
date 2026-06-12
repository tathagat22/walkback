//! An `Effect` is a single change an agent made to the world, paired with
//! enough information to reverse it. This is the heart of the whole system:
//! anything that can describe its own inverse — a file, a network call, an
//! email — fits into the same journal and the same one-button rollback.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A reversible (or at least auditable) side effect.
///
/// `Fs*` variants are fully reversible in v0. `HttpMutation` and `Exec` are
/// recorded for audit and carry the shape needed to reverse them later, but
/// are not auto-reversed tonight (network/shell undo is the roadmap moat).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    /// A file that did not exist before the agent acted. Inverse: delete it.
    FileCreate { path: PathBuf },

    /// A file whose prior contents were captured into the blob store.
    /// Inverse: restore those contents.
    FileModify { path: PathBuf, prev_blob: String },

    /// A file that existed and was (or will be) deleted. Inverse: recreate it
    /// from the captured blob.
    FileDelete { path: PathBuf, prev_blob: String },

    /// A network mutation (POST/PUT/PATCH/DELETE). The `compensator` is the
    /// request that reverses it (e.g. a DELETE to undo a POST). Recorded in v0.
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
    /// Can this effect be reversed automatically by the v0 engine?
    pub fn reversible(&self) -> bool {
        matches!(
            self,
            Effect::FileCreate { .. } | Effect::FileModify { .. } | Effect::FileDelete { .. }
        )
    }

    /// The filesystem path this effect concerns, if any.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Effect::FileCreate { path }
            | Effect::FileModify { path, .. }
            | Effect::FileDelete { path, .. } => Some(path),
            _ => None,
        }
    }

    /// A short, human-readable, log-friendly description.
    pub fn describe(&self) -> String {
        match self {
            Effect::FileCreate { path } => format!("created  {}", path.display()),
            Effect::FileModify { path, .. } => format!("modified {}", path.display()),
            Effect::FileDelete { path, .. } => format!("deleted  {}", path.display()),
            Effect::HttpMutation { method, url, .. } => format!("{method:<8} {url}"),
            Effect::Exec { command, .. } => format!("ran      {command}"),
        }
    }
}
