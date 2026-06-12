//! `undo` — the human-facing CLI. Ctrl-Z for AI agents.
//!
//!   undo init                      set up undo in this directory
//!   undo checkpoint [label...]     mark a point you can rewind to
//!   undo track <path>...           capture a file before the agent changes it
//!   undo status                    what's changed since the last checkpoint
//!   undo log                       the full history
//!   undo rollback [checkpoint]     rewind everything since a checkpoint

use std::env;
use std::io;
use std::path::Path;
use std::process::exit;
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
        "log" => cmd_log(),
        "rollback" | "undo" => cmd_rollback(rest),
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
    println!("\x1b[32m✓\x1b[0m initialized undo in {}", cwd.join(".undo").display());
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
    for p in rest {
        let eff = u.track(Path::new(p))?;
        println!("\x1b[32m✓\x1b[0m tracking  {}", eff.describe());
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
            let mark = if e.reversible() { "\x1b[32m⟲\x1b[0m" } else { "\x1b[33m•\x1b[0m" };
            println!("    {mark} {}", e.describe());
        }
        println!("\n  run `undo rollback` to rewind all of it");
    }
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
    println!(
        "\x1b[32m✓\x1b[0m rewound to \x1b[1m{}\x1b[0m",
        report.checkpoint
    );
    for r in &report.reverted {
        println!("  \x1b[32m⟲\x1b[0m {r}");
    }
    for s in &report.skipped {
        println!("  \x1b[33m•\x1b[0m {s}");
    }
    if report.reverted.is_empty() && report.skipped.is_empty() {
        println!("  (nothing to undo)");
    }
    Ok(())
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
         \x20 log                      the full history\n\
         \x20 rollback [checkpoint]    rewind everything since a checkpoint\n\
         \x20 version                  print version"
    );
}
