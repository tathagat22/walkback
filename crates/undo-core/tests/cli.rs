//! End-to-end tests for the Phase 2 auto-capture commands, driving the real
//! compiled `undo` binary exactly as Claude Code and a shell would.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_undo")
}

fn tmp() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("undo-cli-{}-{}", std::process::id(), n));
    fs::create_dir_all(&base).unwrap();
    base
}

fn run(dir: &PathBuf, args: &[&str]) {
    let status = Command::new(bin())
        .current_dir(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "`undo {}` failed", args.join(" "));
}

#[test]
fn undo_run_snapshots_then_reverses() {
    let dir = tmp();
    fs::write(dir.join("data.txt"), b"v1").unwrap();

    // `undo run -- sh -c '...'` snapshots, then runs a command that wrecks the file.
    let status = Command::new(bin())
        .current_dir(&dir)
        .args(["run", "--", "sh", "-c", "printf WRECKED > data.txt"])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "undo run should exit with the command's code"
    );
    assert_eq!(fs::read(dir.join("data.txt")).unwrap(), b"WRECKED");

    run(&dir, &["rollback"]);
    assert_eq!(fs::read(dir.join("data.txt")).unwrap(), b"v1");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn hook_auto_checkpoints_so_edits_are_reversible() {
    let dir = tmp();
    fs::write(dir.join("app.js"), b"GOOD").unwrap();

    let json = format!(
        r#"{{"session_id":"s1","cwd":"{d}","tool_name":"Edit","tool_input":{{"file_path":"{d}/app.js"}}}}"#,
        d = dir.display()
    );

    // Feed the PreToolUse JSON to `undo hook` on stdin, as Claude Code does.
    let mut child = Command::new(bin())
        .current_dir(&dir)
        .arg("hook")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json.as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "the hook must always exit 0 (never block the agent)"
    );

    // The agent (no cooperation) wrecks the file.
    fs::write(dir.join("app.js"), b"BROKEN").unwrap();

    run(&dir, &["rollback"]);
    assert_eq!(
        fs::read(dir.join("app.js")).unwrap(),
        b"GOOD",
        "the hook made the edit reversible with zero agent involvement"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn watch_baseline_makes_any_change_reversible() {
    let dir = tmp();
    fs::write(dir.join("f.txt"), b"BASE").unwrap();

    // `--once` takes the baseline snapshot and returns (no daemon loop), which
    // is the agent-agnostic guarantee: anything that changes afterwards reverses.
    run(&dir, &["watch", "--once"]);

    fs::write(dir.join("f.txt"), b"SOME AGENT EDITED THIS").unwrap();
    fs::write(dir.join("extra.txt"), b"and added this").unwrap();

    run(&dir, &["rollback"]);
    assert_eq!(fs::read(dir.join("f.txt")).unwrap(), b"BASE");
    assert!(!dir.join("extra.txt").exists(), "agent-added file pruned");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn protect_installs_then_unprotect_removes_the_hook() {
    let dir = tmp();
    run(&dir, &["protect"]);

    let settings = fs::read_to_string(dir.join(".claude/settings.local.json")).unwrap();
    assert!(settings.contains("PreToolUse"));
    assert!(settings.contains("Edit|Write|MultiEdit|NotebookEdit|Bash"));
    assert!(
        settings.contains(bin()),
        "hook command should reference the binary"
    );

    // Running protect twice must not duplicate the hook. One matcher per entry.
    run(&dir, &["protect"]);
    let count = fs::read_to_string(dir.join(".claude/settings.local.json"))
        .unwrap()
        .matches("\"matcher\"")
        .count();
    assert_eq!(count, 1, "protect must be idempotent");

    run(&dir, &["unprotect"]);
    let after = fs::read_to_string(dir.join(".claude/settings.local.json")).unwrap();
    assert!(!after.contains(bin()), "unprotect should remove our hook");
    fs::remove_dir_all(&dir).ok();
}
