//! Property test: for *any* sequence of filesystem mutations an agent might
//! make, capturing the project root and then rolling back must restore the tree
//! byte-for-byte — content, structure, and (on unix) permissions.
//!
//! This is the test that earns trust. We don't enumerate scenarios by hand; we
//! generate many randomized ones from fixed seeds (so failures are reproducible)
//! and assert the round-trip is exact every single time.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use undo_core::Undo;

/// A small, deterministic PRNG so failures reproduce from their seed.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn below(&mut self, n: u32) -> u32 {
        ((self.next() >> 33) as u32) % n
    }
    fn bytes(&mut self) -> Vec<u8> {
        let len = self.below(64) as usize;
        (0..len).map(|_| self.below(256) as u8).collect()
    }
}

fn tmp() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("undo-prop-{}-{}", std::process::id(), n));
    fs::create_dir_all(&base).unwrap();
    base
}

/// The candidate paths our synthetic agent is allowed to touch.
const FILES: &[&str] = &[
    "a.txt",
    "b.txt",
    "sub/c.txt",
    "sub/d.txt",
    "sub/deep/e.txt",
    "data.bin",
];

/// A deterministic fingerprint of the whole tree (excluding `.undo`): each
/// file's relative path -> (contents, unix-mode-or-0, is-symlink target).
fn fingerprint(root: &Path) -> BTreeMap<String, (Vec<u8>, u32, Option<PathBuf>)> {
    let mut map = BTreeMap::new();
    walk(root, root, &mut map);
    map
}

fn walk(root: &Path, dir: &Path, map: &mut BTreeMap<String, (Vec<u8>, u32, Option<PathBuf>)>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for ent in entries.flatten() {
        let path = ent.path();
        let name = ent.file_name();
        if name == ".undo" {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            map.insert(rel, (vec![], 0, Some(fs::read_link(&path).unwrap())));
        } else if meta.is_dir() {
            map.insert(rel.clone(), (vec![], mode_of(&meta), None));
            walk(root, &path, map);
        } else {
            let content = fs::read(&path).unwrap_or_default();
            map.insert(rel, (content, mode_of(&meta), None));
        }
    }
}

#[cfg(unix)]
fn mode_of(m: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.mode() & 0o777
}
#[cfg(not(unix))]
fn mode_of(_m: &fs::Metadata) -> u32 {
    0
}

fn create_initial(root: &Path, rng: &mut Rng) {
    for f in FILES {
        if rng.below(2) == 0 {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, rng.bytes()).unwrap();
        }
    }
}

fn mutate(root: &Path, rng: &mut Rng) {
    let steps = 3 + rng.below(12);
    for _ in 0..steps {
        let f = FILES[rng.below(FILES.len() as u32) as usize];
        let p = root.join(f);
        match rng.below(6) {
            0 | 1 => {
                // create or overwrite
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                let _ = fs::write(&p, rng.bytes());
            }
            2 => {
                let _ = fs::remove_file(&p);
            }
            3 => {
                // a brand-new file in a brand-new directory
                let np = root
                    .join("agent_new")
                    .join(format!("g{}.txt", rng.below(3)));
                fs::create_dir_all(np.parent().unwrap()).unwrap();
                let _ = fs::write(&np, rng.bytes());
            }
            4 => {
                let _ = fs::remove_dir_all(root.join("sub/deep"));
            }
            _ => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if p.exists() {
                        let m = [0o600, 0o644, 0o755][rng.below(3) as usize];
                        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(m));
                    }
                }
            }
        }
    }
}

#[test]
fn random_sequences_round_trip_exactly() {
    for seed in 1..=60u64 {
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();
        let mut rng = Rng::new(seed);

        create_initial(&dir, &mut rng);
        let before = fingerprint(&dir);

        u.checkpoint("base").unwrap();
        u.track(&dir).unwrap(); // capture the whole project root

        mutate(&dir, &mut rng);

        let report = u.rollback(None).unwrap();
        assert!(
            report.failed.is_empty(),
            "seed {seed}: rollback reported failures: {:?}",
            report.failed
        );

        let after = fingerprint(&dir);
        assert_eq!(before, after, "seed {seed}: tree was not restored exactly");

        fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn round_trip_then_redo_then_round_trip() {
    for seed in 1..=30u64 {
        let dir = tmp();
        let u = Undo::init(&dir).unwrap();
        let mut rng = Rng::new(seed.wrapping_add(1000));

        create_initial(&dir, &mut rng);
        let before = fingerprint(&dir);
        u.checkpoint("base").unwrap();
        u.track(&dir).unwrap();
        mutate(&dir, &mut rng);
        let after_mutate = fingerprint(&dir);

        u.rollback(None).unwrap();
        assert_eq!(before, fingerprint(&dir), "seed {seed}: rollback mismatch");

        let redo = u.redo().unwrap();
        assert!(redo.failed.is_empty(), "seed {seed}: redo failed");
        assert_eq!(
            after_mutate,
            fingerprint(&dir),
            "seed {seed}: redo did not reproduce the agent's state"
        );

        fs::remove_dir_all(&dir).ok();
    }
}
