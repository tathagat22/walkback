# undo — Ctrl-Z for AI agents

[![CI](https://github.com/tathagat22/agent-undo/actions/workflows/ci.yml/badge.svg)](https://github.com/tathagat22/agent-undo/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

> When you let an AI agent loose on your machine, `undo` records **every change it makes to the real world** and lets you reverse all of it with one command.

The thing stopping people from running agents in full-auto isn't intelligence — it's **fear**. An agent edits 15 files, deletes a folder, runs a migration, fires off an API call. If it screws up, the files are *maybe* recoverable (if you committed to git) — but the deleted folder, the DB row, the sent email, the network call? **No undo exists anywhere.**

`undo` is that undo. Act freely, because everything is reversible.

```
$ undo checkpoint "before the agent runs"
  ✓ checkpoint cp001

  ... agent wipes a secret, deletes auth.ts, dumps junk, POSTs a charge ...

$ undo rollback
  ✓ rewound to cp001
    ⟲ created  experimental.ts
    ⟲ modified auth.ts
    ⟲ modified config.ts
    •  POST https://api.stripe.com/v1/charges (manual — compensating refund recorded)
```

Two seconds later it's like it never happened.

---

## Zero-effort mode: `undo protect`

The catch with any safety tool is that it only helps if it's actually on. So undo can run itself — no agent cooperation, nothing to remember:

```bash
undo protect
```

That installs a Claude Code **PreToolUse hook**. From then on, before the agent's *first* action in any session, undo silently checkpoints and snapshots your project. Edit, Write, Bash — all of it becomes reversible automatically. If the agent makes a mess:

```bash
undo rollback     # rewind the whole session
undo redo         # ...changed your mind
```

The hook **never blocks or slows the agent** (it exits immediately and always allows the tool), and it skips noise like `node_modules`, `target`, and `.git`. To turn it off: `undo unprotect`.

Not using Claude Code? Wrap any command:

```bash
undo run -- npm run risky-migration    # snapshots first; `undo rollback` reverses it
```

---

## How it works

Every action an agent takes becomes a journal entry that knows **how to reverse itself** — think *git + a flight recorder, but for side effects instead of just files*.

```
checkpoint "before refactor"
  ├─ modified  src/auth.ts        → restore prior contents (byte-perfect)
  ├─ created   src/session.ts     → delete it
  ├─ deleted   legacy/old.ts      → recreate from snapshot
  ├─ ran       npm run migrate    → audited
  └─ POST      api.com/charges    → compensating DELETE recorded
```

Prior file contents are captured into a git-style **content-addressed blob store** (`.undo/objects/`), so even large and binary files restore exactly. Rollback walks the effects since a checkpoint, applies each one's inverse in reverse order, and truncates the journal back to that point.

## Architecture

A polyglot system with a real native boundary:

```
┌─────────────────────────────┐
│  TypeScript  (agent surface) │   MCP server  ·  programmatic API
│     src/mcp.ts  ·  index.ts  │
└──────────────┬──────────────┘
               │  NAPI-RS (in-process, no subprocess)
┌──────────────┴──────────────┐
│  Rust  (the engine)          │   Effect · Journal · blob store · rollback
│   crates/undo-core           │   + standalone `undo` CLI
│   crates/undo-napi           │
└─────────────────────────────┘
```

- **Rust** owns the part that touches your filesystem and has to be fast and trustworthy.
- **TypeScript** owns the agent-facing MCP server and ergonomics.
- **NAPI-RS** bridges them in-process — TS calls Rust directly, no IPC.

## Install

**The CLI** (Rust):

```bash
cargo install --path crates/undo-core
undo --help
```

**The MCP server** (for agents like Claude Code):

```bash
npm install            # installs deps
npm run build:engine   # builds the native Rust engine
npm run build          # compiles the TypeScript
```

## Use it with Claude Code

Add to your `.mcp.json`:

```json
{
  "mcpServers": {
    "undo": {
      "command": "node",
      "args": ["/absolute/path/to/agent-undo/dist/mcp.js"]
    }
  }
}
```

Then tell your agent: *"Before you start, checkpoint with undo and track every file you touch."* The agent calls `undo_checkpoint` and `undo_track` as it works; you call `undo_rollback` (or ask it to) if anything goes sideways.

### MCP tools

| Tool | What it does |
|---|---|
| `undo_init` | Set up the time machine in a project |
| `undo_checkpoint` | Mark a point you can rewind to |
| `undo_track` | Capture files before the agent changes them |
| `undo_record_http` | Log a network mutation + its compensating request |
| `undo_status` | What's changed since the checkpoint |
| `undo_log` | The full history |
| `undo_rollback` | Rewind everything since a checkpoint |
| `undo_redo` | Undo the last rollback |

## CLI

```
undo init                      set up undo in this directory
undo checkpoint [label]        mark a point you can rewind to
undo track <path>...           capture a file before the agent changes it
undo status                    what's changed since the last checkpoint
undo log                       the full history
undo rollback [checkpoint]     rewind everything since a checkpoint
undo redo                      undo the last rollback

undo protect                   install the Claude Code auto-capture hook
undo unprotect                 remove the hook
undo run -- <command>          snapshot, then run any command reversibly
```

## Why you can trust it

A universal undo is only worth anything if it's *correct under pressure*. The engine is built for that:

- **Crash-safe** — the journal and state are written with write-temp-then-rename (atomic on POSIX). A crash never leaves a half-written history.
- **Rollback integrity** — if any single step of a rollback fails, the journal is left intact and the whole thing is safe to retry. It never reports success while leaving files unrestored.
- **Whole directory trees** — `track` captures directories recursively; rollback restores deleted trees, and prunes files the agent *added* to a tracked folder.
- **Byte-perfect fidelity** — file contents via a content-addressed store, plus unix permissions, the executable bit, and mtime. Symlinks are restored as links, never their targets.
- **Concurrency-safe** — mutating operations take an exclusive lock, so an agent and a human (or a multi-agent fleet) can't corrupt the journal.
- **Sandboxed** — refuses to touch anything outside the project root (no `../` traversal), refuses to capture its own `.undo`, and adds `.undo/` to `.gitignore` so snapshots of your secrets never get committed.
- **Redo** — changed your mind? `undo redo` re-applies what a rollback reversed and re-extends the history so you can roll back again.

This isn't asserted, it's tested: alongside unit tests for each property, a **property test** runs dozens of randomized mutation sequences each run and asserts the tree round-trips byte-for-byte, and a **concurrency test** hammers one journal from many threads and asserts no corruption or duplicate sequence numbers. CI runs the whole suite on Linux and macOS.

## Try the demo

```bash
npm run demo        # in-process Rust engine: agent trashes a project, one button restores it
npm run mcp -- ...  # or run the MCP server on stdio
npx tsx demo/mcp-smoke.ts   # drives the real MCP server through a full scenario
```

## What's reversible today, and what's next

**Today (v0):** filesystem create / modify / delete, fully reversed. Shell commands and HTTP mutations are recorded (with compensating requests) and surfaced for manual handling.

**Roadmap — the part nobody has built:**

- **HTTP mutation reversal** — auto-run the compensating request to undo a network call
- **Email undo** — recall/delete within the provider's window
- **Database journaling** — capture inverse SQL, roll back a migration
- **Cloud-resource reversal** — tear down infra the agent spun up
- **Selective undo** — reverse just the email, keep the file edits
- **`undo diff`** — "show me everything the AI changed," reviewable like a PR
- **Redo** — roll forward again after rolling back

The novel core is the `Effect` abstraction: anything that can describe its own inverse plugs into the *same* journal and the *same* one-button rollback. Filesystem-only undo exists; **heterogeneous, cross-system undo does not.** That uniform reversibility layer is the point.

## License

MIT
