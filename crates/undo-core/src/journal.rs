//! The append-only journal, the rollback executor, and redo.
//!
//! Layout under a `.undo/` directory at the project root:
//!   journal.jsonl   — one JSON row per line (checkpoints + effects); source of truth
//!   state.json      — small O(1) cache: seq, current checkpoint, tracked set
//!   redo.json       — after-state captured at the last rollback (enables `undo redo`)
//!   objects/<hash>  — captured prior file contents (see `store`)
//!   lock            — flock target serializing concurrent writers
//!
//! Durability rules that make this trustworthy:
//!   - Every whole-file write (journal rewrite, state, redo) is write-temp-then-rename,
//!     which is atomic on POSIX. A crash never leaves a half-written journal.
//!   - A rollback only truncates the journal if *every* inverse succeeded. If any
//!     step fails, the journal is left intact so the user can safely retry.
//!   - Mutating operations hold an exclusive flock, so an agent and a human CLI
//!     can't corrupt the journal by writing at the same time.

use crate::effect::Effect;
use crate::meta::{self, Meta};
use crate::store::Store;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
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

/// Small persisted cache so common operations don't re-parse the whole journal.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Highest effect sequence number issued.
    seq: u64,
    /// Monotonic high-water mark for checkpoint ids — never reused, even after rollback.
    checkpoint_high: u64,
    /// The checkpoint new effects attach to.
    current_checkpoint: Option<String>,
    /// Absolute paths already captured under the current checkpoint (dedup).
    tracked: Vec<String>,
}

/// After-state captured at rollback time so the rollback itself can be undone.
#[derive(Debug, Serialize, Deserialize)]
struct RedoLog {
    rows: Vec<Row>,
    after: Vec<AfterSnap>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AfterSnap {
    path: PathBuf,
    state: AfterState,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum AfterState {
    File { blob: String, meta: Meta },
    Symlink { target: PathBuf },
    Dir { mode: u32 },
    Absent,
}

/// What's changed since the last checkpoint.
#[derive(Debug, Serialize)]
pub struct Status {
    pub checkpoint: Option<(String, String)>,
    pub effects: Vec<Effect>,
}

/// What a rollback did.
#[derive(Debug, Serialize)]
pub struct RollbackReport {
    pub checkpoint: String,
    pub reverted: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
}

/// What a redo did.
#[derive(Debug, Serialize)]
pub struct RedoReport {
    pub restored: Vec<String>,
    pub failed: Vec<String>,
}

/// A handle on one project's undo history.
pub struct Undo {
    workdir: PathBuf,
    root: PathBuf,
    store: Store,
}

impl Undo {
    pub fn dir_name() -> &'static str {
        ".undo"
    }

    fn at(workdir: &Path) -> Undo {
        let root = workdir.join(Self::dir_name());
        let store = Store::new(root.join("objects"));
        Undo {
            workdir: workdir.to_path_buf(),
            root,
            store,
        }
    }

    /// Create a fresh `.undo` under `workdir`, and protect the user from
    /// committing captured secrets by adding `.undo/` to `.gitignore`.
    pub fn init(workdir: &Path) -> io::Result<Undo> {
        let u = Undo::at(workdir);
        fs::create_dir_all(&u.root)?;
        u.store.ensure()?;
        if !u.journal_path().exists() {
            atomic_write(&u.journal_path(), b"")?;
        }
        if !u.state_path().exists() {
            u.save_state(&State::default())?;
        }
        u.ensure_gitignore();
        Ok(u)
    }

    /// Walk up from `start` to find an existing `.undo` directory.
    pub fn discover(start: &Path) -> io::Result<Option<Undo>> {
        let mut cur = Some(start.to_path_buf());
        while let Some(dir) = cur {
            if dir.join(Self::dir_name()).is_dir() {
                let u = Undo::at(&dir);
                u.ensure_state()?;
                return Ok(Some(u));
            }
            cur = dir.parent().map(|p| p.to_path_buf());
        }
        Ok(None)
    }

    fn journal_path(&self) -> PathBuf {
        self.root.join("journal.jsonl")
    }
    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }
    fn redo_path(&self) -> PathBuf {
        self.root.join("redo.json")
    }

    /// Acquire the exclusive cross-process lock for the duration of a mutation.
    fn lock(&self) -> io::Result<File> {
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.root.join("lock"))?;
        f.lock_exclusive()?;
        Ok(f)
    }

    // ---- journal i/o -------------------------------------------------------

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

    fn append_row(&self, row: &Row) -> io::Result<()> {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.journal_path())?;
        writeln!(f, "{}", serde_json::to_string(row).map_err(invalid_data)?)?;
        f.sync_all()?;
        Ok(())
    }

    fn rewrite_journal(&self, rows: &[Row]) -> io::Result<()> {
        let mut buf = String::new();
        for r in rows {
            buf.push_str(&serde_json::to_string(r).map_err(invalid_data)?);
            buf.push('\n');
        }
        atomic_write(&self.journal_path(), buf.as_bytes())
    }

    // ---- state -------------------------------------------------------------

    fn load_state(&self) -> io::Result<State> {
        match fs::read(self.state_path()) {
            Ok(b) => Ok(serde_json::from_slice(&b).unwrap_or_default()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e),
        }
    }

    fn save_state(&self, s: &State) -> io::Result<()> {
        let b = serde_json::to_vec_pretty(s).map_err(invalid_data)?;
        atomic_write(&self.state_path(), &b)
    }

    /// Recompute the state cache from the journal (after truncation or migration).
    fn rebuild_state(&self, rows: &[Row]) -> io::Result<()> {
        let prev_high = self.load_state().map(|s| s.checkpoint_high).unwrap_or(0);
        let mut st = State {
            checkpoint_high: prev_high,
            ..Default::default()
        };
        for r in rows {
            match r {
                Row::Checkpoint { id, .. } => {
                    st.current_checkpoint = Some(id.clone());
                    st.tracked.clear();
                    if let Some(n) = parse_cp(id) {
                        st.checkpoint_high = st.checkpoint_high.max(n);
                    }
                }
                Row::Effect { seq, effect, .. } => {
                    st.seq = st.seq.max(*seq);
                    if let Some(p) = effect.path() {
                        st.tracked.push(p.to_string_lossy().to_string());
                    }
                }
            }
        }
        self.save_state(&st)
    }

    fn ensure_state(&self) -> io::Result<()> {
        if !self.state_path().exists() {
            let rows = self.rows()?;
            self.rebuild_state(&rows)?;
        }
        Ok(())
    }

    /// Ensure a checkpoint exists to attach effects to, creating one if needed.
    /// Mutates `st` (caller persists) and appends the checkpoint row if created.
    fn ensure_cp(&self, st: &mut State) -> io::Result<String> {
        if let Some(id) = &st.current_checkpoint {
            return Ok(id.clone());
        }
        st.checkpoint_high += 1;
        let id = format!("cp{:03}", st.checkpoint_high);
        st.current_checkpoint = Some(id.clone());
        st.tracked.clear();
        self.append_row(&Row::Checkpoint {
            id: id.clone(),
            label: "auto".to_string(),
            ts: now_millis(),
        })?;
        Ok(id)
    }

    // ---- public mutations --------------------------------------------------

    /// Mark a point in time. Returns the checkpoint id.
    pub fn checkpoint(&self, label: &str) -> io::Result<String> {
        let _lock = self.lock()?;
        self.clear_redo();
        let mut st = self.load_state()?;
        st.checkpoint_high += 1;
        let id = format!("cp{:03}", st.checkpoint_high);
        st.current_checkpoint = Some(id.clone());
        st.tracked.clear();
        self.append_row(&Row::Checkpoint {
            id: id.clone(),
            label: label.to_string(),
            ts: now_millis(),
        })?;
        self.save_state(&st)?;
        Ok(id)
    }

    pub fn current_checkpoint(&self) -> io::Result<Option<String>> {
        Ok(self.load_state()?.current_checkpoint)
    }

    /// The project root (the directory containing `.undo`).
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Capture a path (and, if it's a directory, everything under it) before the
    /// agent changes it. Returns the effects recorded. New forward activity here
    /// invalidates any pending redo.
    pub fn track(&self, path: &Path) -> io::Result<Vec<Effect>> {
        let _lock = self.lock()?;
        self.clear_redo();
        let mut st = self.load_state()?;
        let abs = self.resolve(path);
        self.guard(&abs)?;

        // Cheap idempotency: if this exact path is already captured under the
        // current checkpoint, re-tracking is a no-op. This is what makes
        // re-tracking the project root (every Bash / session) O(1) instead of
        // re-hashing the whole tree.
        let abs_key = abs.to_string_lossy().to_string();
        if st.tracked.iter().any(|t| t == &abs_key) {
            return Ok(vec![]);
        }

        let cp = self.ensure_cp(&mut st)?;

        let mut effects = vec![];
        self.snapshot_path(&abs, &mut effects)?;

        let mut tracked: BTreeSet<String> = st.tracked.iter().cloned().collect();
        let mut recorded = vec![];
        for e in effects {
            let key = e
                .path()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if !tracked.insert(key) {
                continue; // already captured under this checkpoint
            }
            st.seq += 1;
            self.append_row(&Row::Effect {
                seq: st.seq,
                checkpoint: cp.clone(),
                ts: now_millis(),
                agent: None,
                effect: e.clone(),
            })?;
            recorded.push(e);
        }
        st.tracked = tracked.into_iter().collect();
        self.save_state(&st)?;
        Ok(recorded)
    }

    /// Record an arbitrary effect (used by the MCP/NAPI layer for http/exec).
    pub fn record(&self, effect: Effect, agent: Option<String>) -> io::Result<()> {
        let _lock = self.lock()?;
        self.clear_redo();
        let mut st = self.load_state()?;
        let cp = self.ensure_cp(&mut st)?;
        st.seq += 1;
        self.append_row(&Row::Effect {
            seq: st.seq,
            checkpoint: cp,
            ts: now_millis(),
            agent,
            effect,
        })?;
        self.save_state(&st)?;
        Ok(())
    }

    // ---- reads -------------------------------------------------------------

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

    pub fn log(&self) -> io::Result<Vec<Row>> {
        self.rows()
    }

    pub fn can_redo(&self) -> bool {
        self.redo_path().exists()
    }

    // ---- rollback ----------------------------------------------------------

    /// Reverse every effect recorded after `target` (or the latest checkpoint if
    /// `None`). The journal is only truncated if *all* inverses succeed; on any
    /// hard failure it's left intact so a retry is safe.
    pub fn rollback(&self, target: Option<&str>) -> io::Result<RollbackReport> {
        let _lock = self.lock()?;
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

        let after_rows: Vec<Row> = rows[cp_idx + 1..].to_vec();
        let effects: Vec<Effect> = after_rows
            .iter()
            .filter_map(|r| match r {
                Row::Effect { effect, .. } => Some(effect.clone()),
                _ => None,
            })
            .collect();

        // Capture current ("after") state so the rollback can itself be undone.
        let after = self.capture_after(&effects)?;

        let mut reverted = vec![];
        let mut skipped = vec![];
        let mut failed = vec![];
        for eff in effects.iter().rev() {
            match self.invert(eff) {
                Ok(Some(msg)) => reverted.push(msg),
                Ok(None) => skipped.push(format!("{} (manual)", eff.describe())),
                Err(e) => failed.push(format!("{} (error: {e})", eff.describe())),
            }
        }

        if failed.is_empty() {
            self.rewrite_journal(&rows[..=cp_idx])?;
            self.rebuild_state(&rows[..=cp_idx])?;
            self.save_redo(&RedoLog {
                rows: after_rows,
                after,
            })?;
        }
        // If anything failed, the journal is untouched: retrying rollback is safe.

        Ok(RollbackReport {
            checkpoint: cp_id,
            reverted,
            skipped,
            failed,
        })
    }

    /// Undo the last rollback: restore the agent's changes and re-extend the
    /// journal so you can roll back again.
    pub fn redo(&self) -> io::Result<RedoReport> {
        let _lock = self.lock()?;
        let redo = self
            .load_redo()?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "nothing to redo"))?;

        let mut snaps = redo.after;
        snaps.sort_by_key(order_key);

        let mut restored = vec![];
        let mut failed = vec![];
        for s in &snaps {
            match self.apply_after(s) {
                Ok(msg) => restored.push(msg),
                Err(e) => failed.push(format!("{} (error: {e})", s.path.display())),
            }
        }

        if failed.is_empty() {
            for r in &redo.rows {
                self.append_row(r)?;
            }
            let rows = self.rows()?;
            self.rebuild_state(&rows)?;
            self.clear_redo();
        }

        Ok(RedoReport { restored, failed })
    }

    // ---- snapshot / invert / redo internals --------------------------------

    fn snapshot_path(&self, abs: &Path, out: &mut Vec<Effect>) -> io::Result<()> {
        match fs::symlink_metadata(abs) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                out.push(Effect::PathCreate {
                    path: abs.to_path_buf(),
                });
                Ok(())
            }
            Err(e) => Err(e),
            Ok(m) => {
                let ft = m.file_type();
                if ft.is_symlink() {
                    let target = fs::read_link(abs)?;
                    out.push(Effect::Symlink {
                        path: abs.to_path_buf(),
                        target,
                    });
                } else if ft.is_dir() {
                    let mode = meta::capture(abs)?.mode;
                    let mut entries = vec![];
                    let mut children = vec![];
                    for ent in fs::read_dir(abs)? {
                        let ent = ent?;
                        let child = ent.path();
                        if child == self.root || is_ignored_name(&ent.file_name()) {
                            continue; // never descend into .undo or noise dirs
                        }
                        entries.push(ent.file_name().to_string_lossy().to_string());
                        children.push(child);
                    }
                    entries.sort();
                    out.push(Effect::Dir {
                        path: abs.to_path_buf(),
                        mode,
                        entries,
                    });
                    for c in children {
                        self.snapshot_path(&c, out)?;
                    }
                } else {
                    let blob = self.store.put_file(abs)?;
                    let meta = meta::capture(abs)?;
                    out.push(Effect::File {
                        path: abs.to_path_buf(),
                        prev_blob: blob,
                        meta,
                    });
                }
                Ok(())
            }
        }
    }

    /// Apply a single effect's inverse. Ok(Some(msg)) = reverted with a label,
    /// Ok(None) = audit-only / manual, Err = hard failure.
    fn invert(&self, eff: &Effect) -> io::Result<Option<String>> {
        match eff {
            Effect::PathCreate { path } => {
                remove_any(path)?;
                Ok(Some(format!("removed  {}", path.display())))
            }
            Effect::File {
                path,
                prev_blob,
                meta,
            } => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                remove_if_incompatible(path)?;
                let data = self.store.get(prev_blob)?;
                atomic_write(path, &data)?;
                meta::apply(path, *meta)?;
                Ok(Some(format!("restored {}", path.display())))
            }
            Effect::Symlink { path, target } => {
                remove_any(path)?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                symlink(target, path)?;
                Ok(Some(format!("relinked {}", path.display())))
            }
            Effect::Dir {
                path,
                mode,
                entries,
            } => {
                fs::create_dir_all(path)?;
                meta::set_mode(path, *mode)?;
                let keep: BTreeSet<&str> = entries.iter().map(String::as_str).collect();
                for ent in fs::read_dir(path)? {
                    let ent = ent?;
                    let child = ent.path();
                    if child == self.root || is_ignored_name(&ent.file_name()) {
                        continue; // never prune .undo or ignored dirs (node_modules, etc.)
                    }
                    let name = ent.file_name().to_string_lossy().to_string();
                    if !keep.contains(name.as_str()) {
                        remove_any(&child)?; // prune what the agent added
                    }
                }
                Ok(Some(format!("dir      {}", path.display())))
            }
            Effect::HttpMutation { .. } | Effect::Exec { .. } => Ok(None),
        }
    }

    /// Snapshot the current ("after") state of everything a rollback is about to
    /// touch, so the rollback can itself be undone. This is the blast radius:
    /// every effect path *and* — for tracked directories — their current
    /// children, since rollback prunes agent-added files that have no effect of
    /// their own. Without this, redo couldn't recreate what rollback pruned.
    fn capture_after(&self, effects: &[Effect]) -> io::Result<Vec<AfterSnap>> {
        let mut seen = BTreeSet::new();
        let mut out = vec![];
        for e in effects {
            if let Some(p) = e.path() {
                self.capture_after_path(p, &mut seen, &mut out)?;
            }
        }
        Ok(out)
    }

    fn capture_after_path(
        &self,
        p: &Path,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<AfterSnap>,
    ) -> io::Result<()> {
        if !seen.insert(p.to_string_lossy().to_string()) {
            return Ok(());
        }
        let state = match fs::symlink_metadata(p) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => AfterState::Absent,
            Err(e) => return Err(e),
            Ok(m) => {
                if m.file_type().is_symlink() {
                    AfterState::Symlink {
                        target: fs::read_link(p)?,
                    }
                } else if m.is_dir() {
                    for ent in fs::read_dir(p)? {
                        let ent = ent?;
                        let child = ent.path();
                        if child == self.root || is_ignored_name(&ent.file_name()) {
                            continue;
                        }
                        self.capture_after_path(&child, seen, out)?;
                    }
                    AfterState::Dir {
                        mode: meta::capture(p)?.mode,
                    }
                } else {
                    AfterState::File {
                        blob: self.store.put_file(p)?,
                        meta: meta::capture(p)?,
                    }
                }
            }
        };
        out.push(AfterSnap {
            path: p.to_path_buf(),
            state,
        });
        Ok(())
    }

    fn apply_after(&self, s: &AfterSnap) -> io::Result<String> {
        match &s.state {
            AfterState::Absent => {
                remove_any(&s.path)?;
                Ok(format!("removed  {}", s.path.display()))
            }
            AfterState::File { blob, meta } => {
                if let Some(parent) = s.path.parent() {
                    fs::create_dir_all(parent)?;
                }
                remove_if_incompatible(&s.path)?;
                let data = self.store.get(blob)?;
                atomic_write(&s.path, &data)?;
                meta::apply(&s.path, *meta)?;
                Ok(format!("restored {}", s.path.display()))
            }
            AfterState::Symlink { target } => {
                remove_any(&s.path)?;
                symlink(target, &s.path)?;
                Ok(format!("relinked {}", s.path.display()))
            }
            AfterState::Dir { mode } => {
                fs::create_dir_all(&s.path)?;
                meta::set_mode(&s.path, *mode)?;
                Ok(format!("dir      {}", s.path.display()))
            }
        }
    }

    // ---- redo persistence --------------------------------------------------

    fn save_redo(&self, redo: &RedoLog) -> io::Result<()> {
        let b = serde_json::to_vec(redo).map_err(invalid_data)?;
        atomic_write(&self.redo_path(), &b)
    }

    fn load_redo(&self) -> io::Result<Option<RedoLog>> {
        match fs::read(self.redo_path()) {
            Ok(b) => Ok(serde_json::from_slice(&b).ok()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn clear_redo(&self) {
        let _ = fs::remove_file(self.redo_path());
    }

    // ---- path safety -------------------------------------------------------

    fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workdir.join(path)
        }
    }

    /// Refuse to touch anything outside the project root, or inside `.undo`.
    fn guard(&self, abs: &Path) -> io::Result<()> {
        let norm = lexical_normalize(abs);
        let wd = lexical_normalize(&self.workdir);
        if !norm.starts_with(&wd) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to track a path outside the project: {}",
                    abs.display()
                ),
            ));
        }
        if norm.starts_with(lexical_normalize(&self.root)) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to track undo's own .undo directory",
            ));
        }
        Ok(())
    }

    fn ensure_gitignore(&self) {
        let gi = self.workdir.join(".gitignore");
        let contents = fs::read_to_string(&gi).unwrap_or_default();
        if contents
            .lines()
            .any(|l| matches!(l.trim(), ".undo" | ".undo/" | "/.undo" | "/.undo/"))
        {
            return;
        }
        let mut next = contents;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(
            "# agent-undo: snapshots of your files (may contain secrets) — never commit\n.undo/\n",
        );
        let _ = fs::write(&gi, next); // best-effort; never fail init over this
    }
}

// ---- free helpers ----------------------------------------------------------

fn order_key(s: &AfterSnap) -> (u8, isize) {
    let depth = s.path.components().count() as isize;
    match s.state {
        // Recreate parents before children; remove children before parents.
        AfterState::Absent => (1, -depth),
        _ => (0, depth),
    }
}

/// Directory names that are never captured by undo (and never pruned on
/// rollback). These are noise — regenerable build output, dependency caches,
/// VCS internals — that would bloat snapshots and slow whole-project tracking
/// to no benefit. Kept as a deliberate, documented default; finer `.gitignore`
/// awareness can layer on top later without changing any call site.
const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".undo",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".gradle",
    ".idea",
    ".cargo",
    "vendor",
];

fn is_ignored_name(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|n| IGNORED_DIRS.contains(&n))
}

/// True if any component of `path` is an ignored directory (e.g. `node_modules`,
/// `.git`, `.undo`). The watcher uses this to filter events with the exact same
/// definition the snapshotter uses, so the two never disagree.
pub fn path_is_ignored(path: &Path) -> bool {
    path.components().any(|c| match c {
        Component::Normal(n) => is_ignored_name(n),
        _ => false,
    })
}

fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) => {
            if m.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path) // regular file or symlink (not followed)
            }
        }
    }
}

/// If a directory or symlink occupies a path where we need to write a regular
/// file, clear it first.
fn remove_if_incompatible(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() || m.file_type().is_symlink() => remove_any(path),
        _ => Ok(()),
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Write a whole file atomically: temp file in the same dir, then rename.
fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), now_nanos()));
    {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Lexically resolve `.` and `..` without touching the filesystem (so it works
/// for paths that don't exist yet and can't be fooled by `../` traversal).
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn parse_cp(id: &str) -> Option<u64> {
    id.strip_prefix("cp").and_then(|n| n.parse().ok())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn invalid_data(e: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}
