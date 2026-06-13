//! Concurrency test: many writers hammering the same `.undo` at once (an agent
//! plus a human CLI, or a multi-agent fleet) must never corrupt the journal or
//! hand out duplicate sequence numbers. The exclusive lock is what guarantees it.

use std::fs;
use std::path::PathBuf;
use std::thread;
use undo_core::{Row, Undo};

fn tmp() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("undo-conc-{}-{}", std::process::id(), n));
    fs::create_dir_all(&base).unwrap();
    base
}

#[test]
fn concurrent_writers_dont_corrupt_the_journal() {
    let dir = tmp();
    Undo::init(&dir).unwrap().checkpoint("base").unwrap();

    const THREADS: usize = 8;
    const PER_THREAD: usize = 25;

    let mut handles = vec![];
    for t in 0..THREADS {
        let dir = dir.clone();
        handles.push(thread::spawn(move || {
            // Each thread is an independent writer with its own handle, exactly
            // like a separate process would be.
            let u = Undo::discover(&dir).unwrap().unwrap();
            for i in 0..PER_THREAD {
                let name = format!("t{t}_f{i}.txt");
                let path = dir.join(&name);
                fs::write(&path, format!("{t}:{i}")).unwrap();
                u.track(&path).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // The journal must parse cleanly and every effect must have a unique seq.
    let u = Undo::discover(&dir).unwrap().unwrap();
    let rows = u.log().unwrap();
    let mut seqs: Vec<u64> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Effect { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    seqs.sort_unstable();

    let expected = THREADS * PER_THREAD;
    assert_eq!(seqs.len(), expected, "lost or extra effect rows");

    let before = seqs.len();
    seqs.dedup();
    assert_eq!(
        seqs.len(),
        before,
        "duplicate sequence numbers — the lock failed"
    );

    fs::remove_dir_all(&dir).ok();
}
