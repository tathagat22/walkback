//! `undo diff` — a PR-style view of exactly what the agent changed.
//!
//! undo already holds the *before* state of every file it captured (in the blob
//! store). Comparing that against the current files produces a real diff — the
//! reviewable "here's everything the AI did" surface, sourced from undo's own
//! snapshots rather than git.

use crate::effect::Effect;
use crate::store::Store;
use serde::Serialize;
use similar::{ChangeTag, TextDiff};
use std::fs;
use std::path::Path;

/// One changed path in the diff.
#[derive(Debug, Serialize)]
pub struct DiffEntry {
    pub path: String,
    /// created | modified | deleted | binary | symlink
    pub status: String,
    pub added: usize,
    pub removed: usize,
    /// Unified-diff text (empty for symlink/binary, which carry a note instead).
    pub hunk: String,
}

/// Diff every captured file effect against its current on-disk state.
pub fn diff_effects(effects: &[Effect], store: &Store) -> Vec<DiffEntry> {
    let mut out = vec![];
    for e in effects {
        match e {
            Effect::File {
                path, prev_blob, ..
            } => {
                let before = store.get(prev_blob).unwrap_or_default();
                match fs::read(path) {
                    Ok(after) if after == before => {} // unchanged since capture
                    Ok(after) => out.push(make(path, &before, &after, "modified")),
                    Err(_) => out.push(make(path, &before, &[], "deleted")),
                }
            }
            Effect::PathCreate { path } => {
                if let Ok(after) = fs::read(path) {
                    out.push(make(path, &[], &after, "created"));
                }
            }
            Effect::Symlink { path, target } => out.push(DiffEntry {
                path: path.display().to_string(),
                status: "symlink".into(),
                added: 0,
                removed: 0,
                hunk: format!("→ {}", target.display()),
            }),
            // Directories are reflected by their files; http/exec are surfaced
            // by status/compensate, not the file diff.
            _ => {}
        }
    }
    out
}

fn make(path: &Path, before: &[u8], after: &[u8], status: &str) -> DiffEntry {
    let display = path.display().to_string();
    let (Ok(b), Ok(a)) = (std::str::from_utf8(before), std::str::from_utf8(after)) else {
        return DiffEntry {
            path: display,
            status: "binary".into(),
            added: 0,
            removed: 0,
            hunk: "(binary file changed)".into(),
        };
    };

    let td = TextDiff::from_lines(b, a);
    let mut added = 0;
    let mut removed = 0;
    for c in td.iter_all_changes() {
        match c.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    let hunk = td.unified_diff().context_radius(3).to_string();

    DiffEntry {
        path: display,
        status: status.into(),
        added,
        removed,
        hunk,
    }
}
