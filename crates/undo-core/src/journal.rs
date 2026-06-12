//! The append-only journal and the rollback executor.
//!
//! Everything lives under a `.undo/` directory at the project root:
//!   .undo/journal.jsonl   — one JSON row per line (checkpoints + effects)
//!   .undo/objects/<hash>  — captured prior file contents (see `store`)
//!
//! Rollback = walk the effects recorded after a checkpoint, apply each one's
//! inverse in reverse order, then truncate the journal back to that checkpoint.

use crate::effect::Effect;
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One line in the journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Row {
    Checkpoint {
        id: String,
        label: String,
        ts: u64,
    },
    Effect {
        seq: u64,
        checkpoint: String,
        ts: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        effect: Effect,
    },
}

/// The current pending state since the last checkpoint.
#[derive(Debug, Serialize)]
pub struct Status {
    pub checkpoint: Option<(String, String)>, // (id, label)
    pub effects: Vec<Effect>,
}

/// What a rollback did.
#[derive(Debug, Serialize)]
pub struct RollbackReport {
    pub checkpoint: String,
    pub reverted: Vec<String>,
    pub skipped: Vec<String>,
}

/// A handle on one project's undo history.
pub struct Undo {
    root: PathBuf, // the .undo dir
    store: Store,
}

impl Undo {
    pub fn dir_name() -> &'static str {
        ".undo"
    }

    fn at(workdir: &Path) -> Undo {
        let root = workdir.join(Self::dir_name());
        let store = Store::new(root.join("objects"));
        Undo { root, store }
    }

    /// Create a fresh `.undo` under `workdir`.
    pub fn init(workdir: &Path) -> io::Result<Undo> {
        let u = Undo::at(workdir);
        fs::create_dir_all(&u.root)?;
        u.store.ensure()?;
        if !u.journal_path().exists() {
            fs::write(u.journal_path(), b"")?;
        }
        Ok(u)
    }

    /// Walk up from `start` to find an existing `.undo` directory.
    pub fn discover(start: &Path) -> io::Result<Option<Undo>> {
        let mut cur = Some(start.to_path_buf());
        while let Some(dir) = cur {
            if dir.join(Self::dir_name()).is_dir() {
                return Ok(Some(Undo::at(&dir)));
            }
            cur = dir.parent().map(|p| p.to_path_buf());
        }
        Ok(None)
    }

    fn journal_path(&self) -> PathBuf {
        self.root.join("journal.jsonl")
    }

    /// Read every row in order. Malformed lines are skipped defensively.
    pub fn rows(&self) -> io::Result<Vec<Row>> {
        let file = match fs::File::open(self.journal_path()) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut out = vec![];
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(row) = serde_json::from_str::<Row>(&line) {
                out.push(row);
            }
        }
        Ok(out)
    }

    fn append(&self, row: &Row) -> io::Result<()> {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.journal_path())?;
        writeln!(f, "{}", serde_json::to_string(row).map_err(invalid_data)?)?;
        Ok(())
    }

    fn rewrite(&self, rows: &[Row]) -> io::Result<()> {
        let mut buf = String::new();
        for r in rows {
            buf.push_str(&serde_json::to_string(r).map_err(invalid_data)?);
            buf.push('\n');
        }
        fs::write(self.journal_path(), buf)
    }

    fn next_seq(&self) -> io::Result<u64> {
        let mut max = 0;
        for r in self.rows()? {
            if let Row::Effect { seq, .. } = r {
                max = max.max(seq);
            }
        }
        Ok(max + 1)
    }

    /// Mark a point in time. Returns the checkpoint id.
    pub fn checkpoint(&self, label: &str) -> io::Result<String> {
        let count = self
            .rows()?
            .iter()
            .filter(|r| matches!(r, Row::Checkpoint { .. }))
            .count();
        let id = format!("cp{:03}", count + 1);
        self.append(&Row::Checkpoint {
            id: id.clone(),
            label: label.to_string(),
            ts: now_millis(),
        })?;
        Ok(id)
    }

    /// The id of the most recent checkpoint, if any.
    pub fn current_checkpoint(&self) -> io::Result<Option<String>> {
        Ok(self.rows()?.into_iter().rev().find_map(|r| match r {
            Row::Checkpoint { id, .. } => Some(id),
            _ => None,
        }))
    }

    fn ensure_checkpoint(&self) -> io::Result<String> {
        match self.current_checkpoint()? {
            Some(id) => Ok(id),
            None => self.checkpoint("auto"),
        }
    }

    /// Capture a file's current state so a later change to it can be reversed.
    /// Call this *before* the agent writes/deletes the file. Existing files are
    /// snapshotted (FileModify); not-yet-existing paths record a FileCreate.
    pub fn track(&self, path: &Path) -> io::Result<Effect> {
        let cp = self.ensure_checkpoint()?;
        let abs = resolve(path)?;

        // Don't double-capture the same path within the same checkpoint —
        // the first snapshot is the one we want to restore to.
        if self.already_tracked(&cp, &abs)? {
            // Return the existing effect's shape for reporting.
            let existing = self.rows()?.into_iter().rev().find_map(|r| match r {
                Row::Effect {
                    checkpoint,
                    effect,
                    ..
                } if checkpoint == cp && effect.path() == Some(abs.as_path()) => Some(effect),
                _ => None,
            });
            if let Some(e) = existing {
                return Ok(e);
            }
        }

        let effect = if abs.exists() {
            let blob = self.store.put_file(&abs)?;
            Effect::FileModify {
                path: abs.clone(),
                prev_blob: blob,
            }
        } else {
            Effect::FileCreate { path: abs.clone() }
        };
        self.record(effect.clone(), None)?;
        Ok(effect)
    }

    fn already_tracked(&self, cp: &str, abs: &Path) -> io::Result<bool> {
        Ok(self.rows()?.iter().any(|r| {
            matches!(
                r,
                Row::Effect { checkpoint, effect, .. }
                    if checkpoint == cp && effect.path() == Some(abs)
            )
        }))
    }

    /// Record an arbitrary effect (used by the MCP/NAPI layer for http/exec).
    pub fn record(&self, effect: Effect, agent: Option<String>) -> io::Result<()> {
        let cp = self.ensure_checkpoint()?;
        let seq = self.next_seq()?;
        self.append(&Row::Effect {
            seq,
            checkpoint: cp,
            ts: now_millis(),
            agent,
            effect,
        })
    }

    /// Effects recorded since the most recent checkpoint.
    pub fn status(&self) -> io::Result<Status> {
        let mut checkpoint = None;
        let mut effects = vec![];
        for r in self.rows()? {
            match r {
                Row::Checkpoint { id, label, .. } => {
                    checkpoint = Some((id, label));
                    effects.clear();
                }
                Row::Effect { effect, .. } => effects.push(effect),
            }
        }
        Ok(Status {
            checkpoint,
            effects,
        })
    }

    /// The full journal, oldest first.
    pub fn log(&self) -> io::Result<Vec<Row>> {
        self.rows()
    }

    /// Reverse every effect recorded after `target` (or the latest checkpoint
    /// if `None`), then truncate the journal back to that checkpoint.
    pub fn rollback(&self, target: Option<&str>) -> io::Result<RollbackReport> {
        let rows = self.rows()?;
        let cp_idx = match target {
            Some(id) => rows
                .iter()
                .position(|r| matches!(r, Row::Checkpoint { id: cid, .. } if cid == id)),
            None => rows
                .iter()
                .rposition(|r| matches!(r, Row::Checkpoint { .. })),
        }
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no matching checkpoint to roll back to",
            )
        })?;

        let cp_id = match &rows[cp_idx] {
            Row::Checkpoint { id, .. } => id.clone(),
            _ => unreachable!(),
        };

        // Effects after the checkpoint, in the order they happened.
        let effects: Vec<&Effect> = rows[cp_idx + 1..]
            .iter()
            .filter_map(|r| match r {
                Row::Effect { effect, .. } => Some(effect),
                _ => None,
            })
            .collect();

        let mut reverted = vec![];
        let mut skipped = vec![];
        // Reverse order: undo the most recent change first.
        for eff in effects.iter().rev() {
            match self.invert(eff) {
                Ok(true) => reverted.push(eff.describe()),
                Ok(false) => skipped.push(format!("{} (manual)", eff.describe())),
                Err(e) => skipped.push(format!("{} (error: {e})", eff.describe())),
            }
        }

        // Truncate the journal: keep everything up to and including the checkpoint.
        self.rewrite(&rows[..=cp_idx])?;

        Ok(RollbackReport {
            checkpoint: cp_id,
            reverted,
            skipped,
        })
    }

    /// Apply a single effect's inverse. Returns Ok(true) if reversed,
    /// Ok(false) if it is audit-only / requires manual handling.
    fn invert(&self, eff: &Effect) -> io::Result<bool> {
        match eff {
            Effect::FileCreate { path } => {
                if path.exists() {
                    fs::remove_file(path)?;
                }
                Ok(true)
            }
            Effect::FileModify { path, prev_blob }
            | Effect::FileDelete { path, prev_blob } => {
                let data = self.store.get(prev_blob)?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, data)?;
                Ok(true)
            }
            // Network/shell reversal is the roadmap; recorded but not auto-run.
            Effect::HttpMutation { .. } | Effect::Exec { .. } => Ok(false),
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn resolve(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn invalid_data(e: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}
