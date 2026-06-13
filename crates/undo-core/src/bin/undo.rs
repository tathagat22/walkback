//! `undo` — the human-facing CLI. Ctrl-Z for AI agents.
//!
//!   undo init                      set up undo in this directory
//!   undo checkpoint [label...]     mark a point you can rewind to
//!   undo track <path>...           capture a file before the agent changes it
//!   undo status                    what's changed since the last checkpoint
//!   undo log                       the full history
//!   undo rollback [checkpoint]     rewind everything since a checkpoint
//!   undo redo                      undo the last rollback

use serde_json::json;
use std::collections::BTreeSet;
use std::env;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;
use undo_core::{Row, Undo};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let rest = if args.len() > 1 { &args[1..] } else { &[] };

    let result = match cmd {
        "init" => cmd_init(),
        "checkpoint" | "cp" => cmd_checkpoint(rest),
        "track" => cmd_track(rest),
        "status" | "st" => cmd_status(),
        "diff" => cmd_diff(),
        "log" => cmd_log(),
        "rollback" | "undo" => cmd_rollback(rest),
        "revert" => cmd_revert(rest),
        "redo" => cmd_redo(),
        "run" => cmd_run(rest),
        "watch" => cmd_watch(rest),
        "protect" => cmd_protect(),
        "unprotect" => cmd_unprotect(),
        "hook" => cmd_hook(),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "version" | "--version" | "-V" => {
            println!("undo {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print_help();
            exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("\x1b[31m✗\x1b[0m {e}");
        exit(1);
    }
}

fn open() -> io::Result<Undo> {
    let cwd = env::current_dir()?;
    Undo::discover(&cwd)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no .undo here — run `undo init` first",
        )
    })
}

fn cmd_init() -> io::Result<()> {
    let cwd = env::current_dir()?;
    Undo::init(&cwd)?;
    println!(
        "\x1b[32m✓\x1b[0m initialized undo in {}",
        cwd.join(".undo").display()
    );
    println!("  \x1b[2madded .undo/ to .gitignore (snapshots may contain secrets)\x1b[0m");
    println!("  next:  undo checkpoint \"before the agent runs\"");
    Ok(())
}

fn cmd_checkpoint(rest: &[String]) -> io::Result<()> {
    let label = if rest.is_empty() {
        "checkpoint".to_string()
    } else {
        rest.join(" ")
    };
    let u = open()?;
    let id = u.checkpoint(&label)?;
    println!("\x1b[32m✓\x1b[0m checkpoint \x1b[1m{id}\x1b[0m  \"{label}\"");
    Ok(())
}

fn cmd_track(rest: &[String]) -> io::Result<()> {
    if rest.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: undo track <path>...",
        ));
    }
    let u = open()?;
    let cwd = env::current_dir()?;
    for p in rest {
        // Resolve relative to where the user actually is, not the project root.
        let abs = cwd.join(p);
        let effects = u.track(&abs)?;
        match effects.len() {
            0 => println!("\x1b[2m·\x1b[0m {p} (already tracked)"),
            1 => println!("\x1b[32m✓\x1b[0m tracking  {}", effects[0].describe()),
            n => println!("\x1b[32m✓\x1b[0m tracking  {} ({n} paths)", p),
        }
    }
    Ok(())
}

fn cmd_status() -> io::Result<()> {
    let u = open()?;
    let st = u.status()?;
    match st.checkpoint {
        Some((id, label)) => println!("on checkpoint \x1b[1m{id}\x1b[0m  \"{label}\""),
        None => {
            println!("no checkpoint yet — run `undo checkpoint`");
            return Ok(());
        }
    }
    if st.effects.is_empty() {
        println!("  (nothing recorded since this checkpoint)");
    } else {
        println!("  {} change(s) since the checkpoint:", st.effects.len());
        for e in &st.effects {
            let mark = if e.reversible() {
                "\x1b[32m⟲\x1b[0m"
            } else {
                "\x1b[33m•\x1b[0m"
            };
            println!("    {mark} {}", e.describe());
        }
        println!("\n  run `undo rollback` to rewind all of it");
    }
    Ok(())
}

fn cmd_diff() -> io::Result<()> {
    let u = open()?;
    let entries = u.diff()?;
    if entries.is_empty() {
        println!("(no changes since the checkpoint)");
        return Ok(());
    }
    let mut total_added = 0;
    let mut total_removed = 0;
    for e in &entries {
        total_added += e.added;
        total_removed += e.removed;
        let badge = match e.status.as_str() {
            "created" => "\x1b[32mnew\x1b[0m",
            "deleted" => "\x1b[31mdeleted\x1b[0m",
            "binary" => "\x1b[33mbinary\x1b[0m",
            "symlink" => "\x1b[36msymlink\x1b[0m",
            _ => "\x1b[33mmodified\x1b[0m",
        };
        println!(
            "\n\x1b[1m{}\x1b[0m  {badge}  \x1b[32m+{}\x1b[0m \x1b[31m-{}\x1b[0m",
            e.path, e.added, e.removed
        );
        for line in e.hunk.lines() {
            let colored = if line.starts_with('+') {
                format!("\x1b[32m{line}\x1b[0m")
            } else if line.starts_with('-') {
                format!("\x1b[31m{line}\x1b[0m")
            } else if line.starts_with("@@") {
                format!("\x1b[36m{line}\x1b[0m")
            } else {
                line.to_string()
            };
            println!("  {colored}");
        }
    }
    println!(
        "\n\x1b[1m{}\x1b[0m file(s) changed, \x1b[32m+{total_added}\x1b[0m \x1b[31m-{total_removed}\x1b[0m",
        entries.len()
    );
    Ok(())
}

fn cmd_log() -> io::Result<()> {
    let u = open()?;
    let rows = u.log()?;
    if rows.is_empty() {
        println!("(empty — no history yet)");
        return Ok(());
    }
    for r in rows {
        match r {
            Row::Checkpoint { id, label, .. } => {
                println!("\x1b[1m● {id}\x1b[0m  \"{label}\"");
            }
            Row::Effect { effect, .. } => {
                let mark = if effect.reversible() { "⟲" } else { "•" };
                println!("  {mark} {}", effect.describe());
            }
        }
    }
    Ok(())
}

fn cmd_rollback(rest: &[String]) -> io::Result<()> {
    let u = open()?;
    let target = rest.first().map(String::as_str);
    let report = u.rollback(target)?;
    if report.failed.is_empty() {
        println!(
            "\x1b[32m✓\x1b[0m rewound to \x1b[1m{}\x1b[0m",
            report.checkpoint
        );
    } else {
        println!(
            "\x1b[31m✗\x1b[0m rollback to \x1b[1m{}\x1b[0m incomplete — journal left intact, safe to retry",
            report.checkpoint
        );
    }
    for r in &report.reverted {
        println!("  \x1b[32m⟲\x1b[0m {r}");
    }
    for s in &report.skipped {
        println!("  \x1b[33m•\x1b[0m {s}");
    }
    for f in &report.failed {
        println!("  \x1b[31m✗\x1b[0m {f}");
    }
    if report.reverted.is_empty() && report.skipped.is_empty() && report.failed.is_empty() {
        println!("  (nothing to undo)");
    } else if report.failed.is_empty() {
        println!("\n  \x1b[2mchanged your mind? `undo redo`\x1b[0m");
    }
    if !report.failed.is_empty() {
        exit(1);
    }
    Ok(())
}

fn cmd_revert(rest: &[String]) -> io::Result<()> {
    if rest.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: undo revert <path>...",
        ));
    }
    let u = open()?;
    let cwd = env::current_dir()?;
    for p in rest {
        let abs = cwd.join(p);
        match u.revert(&abs)? {
            Some(msg) => println!("\x1b[32m✓\x1b[0m {msg}"),
            None => println!("\x1b[2m·\x1b[0m {p} (not tracked — nothing to revert)"),
        }
    }
    Ok(())
}

fn cmd_redo() -> io::Result<()> {
    let u = open()?;
    let report = u.redo()?;
    if report.failed.is_empty() {
        println!("\x1b[32m✓\x1b[0m redid the last rollback");
    } else {
        println!("\x1b[31m✗\x1b[0m redo incomplete");
    }
    for r in &report.restored {
        println!("  \x1b[32m⟲\x1b[0m {r}");
    }
    for f in &report.failed {
        println!("  \x1b[31m✗\x1b[0m {f}");
    }
    if !report.failed.is_empty() {
        exit(1);
    }
    Ok(())
}

/// Open the project's undo, creating it if this is a fresh project.
fn discover_or_init(dir: &Path) -> io::Result<Undo> {
    match Undo::discover(dir)? {
        Some(u) => Ok(u),
        None => Undo::init(dir),
    }
}

/// `undo run -- <command>` — snapshot the whole project, then run a command.
/// Whatever the command does to the working tree is reversible with one
/// `undo rollback`. Pre-state is captured up front, so no filesystem watcher
/// (which could only see changes *after* they happen) is needed.
fn cmd_run(rest: &[String]) -> io::Result<()> {
    let cmd: &[String] = match rest.first() {
        Some(first) if first == "--" => &rest[1..],
        _ => rest,
    };
    if cmd.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: undo run -- <command> [args...]",
        ));
    }
    let cwd = env::current_dir()?;
    let u = discover_or_init(&cwd)?;
    let root = u.workdir().to_path_buf();
    let id = u.checkpoint(&format!("run: {}", cmd.join(" ")))?;
    u.track(&root)?;
    println!("\x1b[2m✓ snapshot {id} taken — `undo rollback` reverses anything below\x1b[0m\n");
    let status = std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(&cwd)
        .status()?;
    exit(status.code().unwrap_or(1));
}

/// `undo watch` — the universal, any-agent safety net. Snapshots the project,
/// then watches the filesystem. Because the one thing every AI agent does —
/// regardless of model, vendor, or IDE — is change files on disk, this works
/// with all of them (Cursor, Copilot, Aider, Windsurf, custom scripts) with
/// zero integration. Everything that changes while watching is reversible with
/// one `undo rollback`.
fn cmd_watch(rest: &[String]) -> io::Result<()> {
    let once = rest.iter().any(|a| a == "--once");
    let cwd = env::current_dir()?;
    let u = discover_or_init(&cwd)?;
    let root = u.workdir().to_path_buf();

    let id = u.checkpoint("watch session")?;
    u.track(&root)?; // baseline: captures pre-state of the whole (filtered) tree

    println!(
        "\x1b[32m✓\x1b[0m watching \x1b[1m{}\x1b[0m  (baseline {id})",
        root.display()
    );
    println!("  \x1b[2many agent's changes here are now reversible — works with any tool\x1b[0m");
    if once {
        return Ok(());
    }
    println!("  \x1b[2mCtrl-C to stop, then `undo rollback` to reverse everything\x1b[0m\n");

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    let _ = ctrlc::set_handler(move || r.store(false, Ordering::SeqCst));

    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .map_err(notify_err)?;
    notify::Watcher::watch(&mut watcher, &root, notify::RecursiveMode::Recursive)
        .map_err(notify_err)?;

    // Debounce: collect changed paths, print them after a quiet moment. We only
    // *report* changes — the baseline snapshot already makes them reversible, so
    // re-tracking here would wrongly protect agent-created files from pruning.
    let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
    let mut total = 0usize;
    while running.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(Ok(event)) => {
                for p in event.paths {
                    if !undo_core::path_is_ignored(&p) {
                        pending.insert(p);
                    }
                }
            }
            Ok(Err(_)) => {}
            Err(RecvTimeoutError::Timeout) => {
                for p in std::mem::take(&mut pending) {
                    let rel = p.strip_prefix(&root).unwrap_or(&p);
                    println!("  \x1b[2m~\x1b[0m {}", rel.display());
                    total += 1;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    println!("\n\x1b[32m✓\x1b[0m stopped — {total} change event(s) captured under {id}");
    println!(
        "  \x1b[1mundo rollback\x1b[0m reverses everything since the baseline, or just leave it"
    );
    Ok(())
}

/// `undo protect` — install a Claude Code PreToolUse hook so every session is
/// auto-checkpointed. Zero effort: the agent doesn't have to cooperate.
fn cmd_protect() -> io::Result<()> {
    let cwd = env::current_dir()?;
    discover_or_init(&cwd)?;
    let root = Undo::discover(&cwd)?
        .map(|u| u.workdir().to_path_buf())
        .unwrap_or(cwd);

    let exe = env::current_exe()?;
    let command = format!("\"{}\" hook", exe.display());
    let settings_path = settings_path(&root);
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut settings = read_json(&settings_path)?;
    if !settings.is_object() {
        settings = json!({});
    }
    let arr = pretooluse_array(&mut settings);
    let already = arr
        .iter()
        .any(|e| entry_command(e).as_deref() == Some(command.as_str()));
    if already {
        println!(
            "\x1b[32m✓\x1b[0m already protected (hook present in {})",
            settings_path.display()
        );
        return Ok(());
    }
    arr.push(json!({
        "matcher": "Edit|Write|MultiEdit|NotebookEdit|Bash",
        "hooks": [ { "type": "command", "command": command } ]
    }));
    write_json(&settings_path, &settings)?;

    println!("\x1b[32m✓\x1b[0m undo is now protecting this project");
    println!(
        "  \x1b[2mhook installed in {}\x1b[0m",
        settings_path.display()
    );
    println!("  every Claude Code session is auto-checkpointed before the agent acts");
    println!("  reverse the last session anytime:  \x1b[1mundo rollback\x1b[0m");
    Ok(())
}

/// `undo unprotect` — remove the hook this tool installed.
fn cmd_unprotect() -> io::Result<()> {
    let cwd = env::current_dir()?;
    let root = Undo::discover(&cwd)?
        .map(|u| u.workdir().to_path_buf())
        .unwrap_or(cwd);
    let exe = env::current_exe()?;
    let settings_path = settings_path(&root);
    let mut settings = read_json(&settings_path)?;
    let mut removed = false;
    if let Some(arr) = settings
        .get_mut("hooks")
        .and_then(|h| h.get_mut("PreToolUse"))
        .and_then(|p| p.as_array_mut())
    {
        let before = arr.len();
        arr.retain(|e| !entry_is_ours(e, &exe));
        removed = arr.len() != before;
    }
    if removed {
        write_json(&settings_path, &settings)?;
        println!(
            "\x1b[32m✓\x1b[0m undo hook removed from {}",
            settings_path.display()
        );
    } else {
        println!("nothing to remove (no undo hook found)");
    }
    Ok(())
}

/// `undo hook` — invoked by Claude Code before each tool runs. Reads the
/// PreToolUse JSON on stdin and, once per session, checkpoints + snapshots the
/// project so the whole session is reversible. It NEVER blocks the agent: any
/// error is logged and we still exit 0.
fn cmd_hook() -> io::Result<()> {
    let mut input = String::new();
    let _ = io::stdin().read_to_string(&mut input);
    if let Err(e) = run_hook(&input) {
        eprintln!("undo hook (non-fatal): {e}");
    }
    Ok(()) // exit 0 — the agent always proceeds
}

fn run_hook(input: &str) -> io::Result<()> {
    let v: serde_json::Value = serde_json::from_str(input).unwrap_or(serde_json::Value::Null);
    let cwd = v
        .get("cwd")
        .and_then(|x| x.as_str())
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(env::current_dir)?;
    let session = v
        .get("session_id")
        .and_then(|x| x.as_str())
        .unwrap_or("default");

    let u = discover_or_init(&cwd)?;
    let root = u.workdir().to_path_buf();
    let sessions = root.join(Undo::dir_name()).join("sessions");
    std::fs::create_dir_all(&sessions)?;
    let marker = sessions.join(sanitize(session));

    // One checkpoint per Claude session.
    if !marker.exists() {
        let short = &session[..session.len().min(8)];
        let id = u.checkpoint(&format!("claude session {short}"))?;
        std::fs::write(&marker, id)?;
    }

    // Always ensure the project is snapshotted under the current checkpoint.
    // This is a cheap no-op when already tracked, and it self-heals after a
    // mid-session rollback (which clears the snapshot but keeps the checkpoint).
    u.track(&root)?;
    Ok(())
}

// ---- small helpers for the hook/protect commands --------------------------

fn settings_path(root: &Path) -> PathBuf {
    // settings.local.json is machine-local and gitignored, so an absolute
    // binary path here never pollutes the committed repo or breaks teammates.
    root.join(".claude").join("settings.local.json")
}

fn read_json(path: &Path) -> io::Result<serde_json::Value> {
    match std::fs::read(path) {
        Ok(b) => Ok(serde_json::from_slice(&b).unwrap_or_else(|_| json!({}))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e),
    }
}

fn write_json(path: &Path, value: &serde_json::Value) -> io::Result<()> {
    let mut body = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    body.push(b'\n');
    std::fs::write(path, body)
}

/// Navigate to `settings.hooks.PreToolUse` as a mutable array, creating the
/// nesting (and repairing non-conforming types) as needed.
fn pretooluse_array(settings: &mut serde_json::Value) -> &mut Vec<serde_json::Value> {
    let obj = settings.as_object_mut().expect("settings is an object");
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let pre = hooks
        .as_object_mut()
        .unwrap()
        .entry("PreToolUse")
        .or_insert_with(|| json!([]));
    if !pre.is_array() {
        *pre = json!([]);
    }
    pre.as_array_mut().unwrap()
}

/// The first command string inside a PreToolUse entry, if any.
fn entry_command(entry: &serde_json::Value) -> Option<String> {
    entry
        .get("hooks")?
        .as_array()?
        .iter()
        .find_map(|h| h.get("command").and_then(|c| c.as_str()).map(String::from))
}

fn entry_is_ours(entry: &serde_json::Value, exe: &Path) -> bool {
    let exe = exe.display().to_string();
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains(&exe) && c.contains("hook"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn notify_err(e: notify::Error) -> io::Error {
    io::Error::other(e)
}

fn print_help() {
    println!(
        "\x1b[1mundo\x1b[0m — Ctrl-Z for AI agents\n\n\
         USAGE\n\
         \x20 undo <command> [args]\n\n\
         COMMANDS\n\
         \x20 init                     set up undo in this directory\n\
         \x20 checkpoint [label]       mark a point you can rewind to\n\
         \x20 track <path>...          capture a file before the agent changes it\n\
         \x20 status                   what's changed since the last checkpoint\n\
         \x20 diff                     a PR-style diff of everything the agent changed\n\
         \x20 log                      the full history\n\
         \x20 rollback [checkpoint]    rewind everything since a checkpoint\n\
         \x20 revert <path>            selectively undo just one file\n\
         \x20 redo                     undo the last rollback\n\
         \n\
         AUTO-CAPTURE (works with any AI agent)\n\
         \x20 watch                    snapshot, then watch the filesystem — reversible for ANY agent\n\
         \x20 run -- <command>         snapshot the project, then run any command reversibly\n\
         \x20 protect                  install a Claude Code hook to auto-checkpoint every session\n\
         \x20 unprotect                remove the Claude Code hook\n\
         \n\
         \x20 version                  print version"
    );
}
