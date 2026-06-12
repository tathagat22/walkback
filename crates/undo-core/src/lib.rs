//! `undo-core` — the reversible side-effect engine behind `undo`,
//! "Ctrl-Z for AI agents".
//!
//! The model is small on purpose:
//!   - [`Effect`] is a change paired with how to reverse it.
//!   - [`Undo`] is an append-only journal of effects, grouped by checkpoint,
//!     plus a rollback executor that replays their inverses.
//!
//! Filesystem effects are fully reversible today. The same journal is built to
//! carry network, email, and database effects (which know their own inverse)
//! without changing the rollback path — that uniform reversibility is the point.

mod effect;
mod journal;
mod store;

pub use effect::{Effect, HttpCompensator};
pub use journal::{RollbackReport, Row, Status, Undo};
pub use store::Store;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "undo-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn rolls_back_modify_create_delete() {
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();

        // A file that exists before the agent runs.
        let kept = dir.join("keep.txt");
        fs::write(&kept, b"ORIGINAL").unwrap();

        u.checkpoint("before agent").unwrap();

        // Agent modifies an existing file.
        u.track(&kept).unwrap();
        fs::write(&kept, b"MUTATED BY AGENT").unwrap();

        // Agent creates a brand-new file.
        let created = dir.join("nested/new.txt");
        u.track(&created).unwrap();
        fs::create_dir_all(created.parent().unwrap()).unwrap();
        fs::write(&created, b"agent made this").unwrap();

        // Sanity: the world is changed.
        assert_eq!(fs::read(&kept).unwrap(), b"MUTATED BY AGENT");
        assert!(created.exists());

        // One button.
        let report = u.rollback(None).unwrap();
        assert_eq!(report.reverted.len(), 2);

        // The world is restored.
        assert_eq!(fs::read(&kept).unwrap(), b"ORIGINAL");
        assert!(!created.exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn double_track_keeps_earliest_snapshot() {
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();
        let f = dir.join("a.txt");
        fs::write(&f, b"v1").unwrap();
        u.checkpoint("c").unwrap();

        u.track(&f).unwrap();
        fs::write(&f, b"v2").unwrap();
        u.track(&f).unwrap(); // should NOT capture v2
        fs::write(&f, b"v3").unwrap();

        u.rollback(None).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"v1");
        fs::remove_dir_all(&dir).ok();
    }
}
