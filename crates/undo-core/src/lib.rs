//! `undo-core` — the reversible side-effect engine behind `undo`,
//! "Ctrl-Z for AI agents".
//!
//! The model is small on purpose:
//!   - [`Effect`] is a change paired with how to reverse it.
//!   - [`Undo`] is an append-only journal of effects, grouped by checkpoint,
//!     plus a rollback executor that replays their inverses — crash-safely,
//!     under a cross-process lock, with directory/permission/symlink fidelity
//!     and a redo stack.
//!
//! Filesystem effects are fully reversible today. The same journal is built to
//! carry network, email, and database effects (which know their own inverse)
//! without changing the rollback path — that uniform reversibility is the point.

mod effect;
mod journal;
mod meta;
mod store;

pub use effect::{Effect, HttpCompensator};
pub use journal::{RedoReport, RollbackReport, Row, Status, Undo};
pub use store::Store;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        // A monotonic counter guarantees uniqueness even when parallel tests
        // hit the same coarse-resolution timestamp.
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("undo-test-{}-{}", std::process::id(), n));
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn rolls_back_modify_create_delete() {
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();
        let kept = dir.join("keep.txt");
        fs::write(&kept, b"ORIGINAL").unwrap();
        u.checkpoint("before agent").unwrap();

        u.track(&kept).unwrap();
        fs::write(&kept, b"MUTATED").unwrap();

        let created = dir.join("nested/new.txt");
        u.track(&created).unwrap();
        fs::create_dir_all(created.parent().unwrap()).unwrap();
        fs::write(&created, b"junk").unwrap();

        u.rollback(None).unwrap();
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
        u.track(&f).unwrap();
        fs::write(&f, b"v3").unwrap();
        u.rollback(None).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"v1");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn restores_a_deleted_directory_tree() {
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();
        fs::create_dir_all(dir.join("src/util")).unwrap();
        fs::write(dir.join("src/main.rs"), b"fn main(){}").unwrap();
        fs::write(dir.join("src/util/log.rs"), b"// log").unwrap();
        u.checkpoint("before").unwrap();

        u.track(&dir.join("src")).unwrap();
        fs::remove_dir_all(dir.join("src")).unwrap();
        assert!(!dir.join("src").exists());

        u.rollback(None).unwrap();
        assert_eq!(fs::read(dir.join("src/main.rs")).unwrap(), b"fn main(){}");
        assert_eq!(fs::read(dir.join("src/util/log.rs")).unwrap(), b"// log");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prunes_files_the_agent_added_to_a_tracked_dir() {
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();
        fs::create_dir_all(dir.join("conf")).unwrap();
        fs::write(dir.join("conf/a.txt"), b"a").unwrap();
        u.checkpoint("before").unwrap();

        u.track(&dir.join("conf")).unwrap();
        fs::write(dir.join("conf/sneaky.txt"), b"added by agent").unwrap();

        u.rollback(None).unwrap();
        assert!(dir.join("conf/a.txt").exists());
        assert!(
            !dir.join("conf/sneaky.txt").exists(),
            "agent-added file should be pruned"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn restores_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();
        let script = dir.join("run.sh");
        fs::write(&script, b"#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        u.checkpoint("c").unwrap();

        u.track(&script).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&script, b"tampered").unwrap();

        u.rollback(None).unwrap();
        let mode = fs::metadata(&script).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "executable bit must be restored");
        assert_eq!(fs::read(&script).unwrap(), b"#!/bin/sh\necho hi\n");
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn restores_a_symlink_not_its_target() {
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();
        fs::write(dir.join("real.txt"), b"real").unwrap();
        std::os::unix::fs::symlink("real.txt", dir.join("link")).unwrap();
        u.checkpoint("c").unwrap();

        u.track(&dir.join("link")).unwrap();
        fs::remove_file(dir.join("link")).unwrap();
        fs::write(dir.join("link"), b"now a regular file").unwrap();

        u.rollback(None).unwrap();
        let meta = fs::symlink_metadata(dir.join("link")).unwrap();
        assert!(meta.file_type().is_symlink(), "should be a symlink again");
        assert_eq!(
            fs::read_link(dir.join("link")).unwrap(),
            PathBuf::from("real.txt")
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn redo_reapplies_the_rollback() {
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();
        let f = dir.join("doc.txt");
        fs::write(&f, b"original").unwrap();
        u.checkpoint("c").unwrap();
        u.track(&f).unwrap();
        fs::write(&f, b"agent edit").unwrap();

        u.rollback(None).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"original");

        let report = u.redo().unwrap();
        assert!(report.failed.is_empty());
        assert_eq!(
            fs::read(&f).unwrap(),
            b"agent edit",
            "redo restores the agent's change"
        );

        // And we can roll back again after redo (journal was re-extended).
        u.rollback(None).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"original");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_paths_outside_the_project() {
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();
        u.checkpoint("c").unwrap();
        let outside = u.track(std::path::Path::new("/etc/hosts"));
        assert!(
            outside.is_err(),
            "tracking outside the project must be refused"
        );
        let traversal = u.track(std::path::Path::new("../../../etc/hosts"));
        assert!(traversal.is_err(), "path traversal must be refused");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writes_gitignore_on_init() {
        let dir = tmp();
        Undo::init(&dir).unwrap();
        let gi = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(gi.contains(".undo/"), "init should gitignore .undo");
        fs::remove_dir_all(&dir).ok();
    }
}
