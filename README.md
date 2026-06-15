<p align="center">
  <img src="docs/banner.svg" alt="walkback — Undo anything your AI agent does" width="620">
</p>

<p align="center">
  <a href="https://github.com/tathagat22/walkback/actions/workflows/ci.yml"><img src="https://github.com/tathagat22/walkback/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://www.npmjs.com/package/@tathagatmaitray/walkback"><img src="https://img.shields.io/npm/v/@tathagatmaitray/walkback?label=npm&color=cb3837" alt="npm"></a>
  <a href="https://crates.io/crates/walkback-core"><img src="https://img.shields.io/crates/v/walkback-core?label=crates.io&color=e6a141" alt="crates.io"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-6366f1" alt="MIT"></a>
  <a href="https://tathagat22.github.io/walkback/"><img src="https://img.shields.io/badge/site-walkback-22d3ee" alt="site"></a>
</p>

<p align="center"><b>When an AI agent goes off the rails, <code>walkback</code> rewinds everything it did to the real world — not just the files.</b></p>

---

Your agent's **file** changes are already recoverable — that's what git is for. But an autonomous agent does far more than edit files: it fires off API calls, charges cards, sends emails, runs migrations, spins up cloud resources. **Git can't see any of that, and nothing reverses it.**

`walkback` is the journal and the rollback for everything git can't track.

```console
$ walkback watch                   # arm it — now the agent's changes are reversible

  ... agent edits 15 files, POSTs a charge, sends an email, drops a table ...

$ walkback diff                    # review exactly what it did
$ walkback rollback                # rewind the files
  ✓ rewound to cp001
```

…and for the things that have no undo anywhere — the charge, the email, the migration, the bucket — the agent records the inverse as it acts, and `walkback` replays it (dry-run gated, so it never fires blind).

## Why not just git?

| | git / editor undo | walkback |
|---|---|---|
| File changes | ✅ | ✅ (byte-for-byte, or via git) |
| A `POST` that charged a card | ❌ | ✅ records a refund compensator |
| A sent email | ❌ | ✅ holds as a draft → true unsend |
| A dropped table / migration | ❌ | ✅ runs the inverse you record |
| A cloud resource it created | ❌ | ✅ runs the teardown you record |
| One audit + one rollback across all of it | ❌ | ✅ |

**Not a git replacement — a second system for everything git can't track.**

## Works with any AI agent

`walkback` is **not tied to any model, vendor, or IDE.** Every agent changes files on disk, so it meets yours at whichever layer is convenient:

| Your setup | Turn it on | Covers |
|---|---|---|
| **Anything** — Cursor, Copilot, Windsurf, Aider, custom scripts, even you | `walkback watch` | Snapshots, then watches the filesystem. Reversible no matter what made the change. |
| **Any CLI agent** | `walkback run -- <agent-cmd>` | Wraps the command; snapshots first, reversible after. |
| **Any MCP client** | add the [MCP server](#mcp-server) | The agent calls `walkback_checkpoint` / `walkback_compensate` / … itself. |
| **Claude Code** | `walkback protect` | Native PreToolUse hook — auto-checkpoints every session, zero effort. |

## Install

The **CLI** (`walkback` binary) works on macOS, Linux, and Windows — no Node required:

```bash
cargo install walkback-core                  # via crates.io (installs the `walkback` binary)
brew install tathagat22/tap/walkback         # via Homebrew
curl -fsSL https://raw.githubusercontent.com/tathagat22/walkback/main/packaging/install.sh | sh
```

The **MCP server** (for MCP clients like Cursor / Claude):

```bash
npx -y @tathagatmaitray/walkback
```

## What it reverses

One consistent model — record a change with its inverse, replay the inverse on rollback — across every domain. Anything that touches the outside world is **dry-run gated**: walkback shows you what it *would* do and never fires blindly.

### 📁 Files — byte-perfect, crash-safe · **CLI, automatic**
Create / modify / delete / directories / symlinks / permissions, all restored exactly from a content-addressed blob store. Plus **redo**, and **selective** per-file revert.

```bash
walkback rollback              # rewind everything since the checkpoint
walkback revert src/auth.ts    # ...or just one file
walkback redo                  # ...changed your mind
```

### 🔍 `walkback diff` — review before you trust · **CLI + MCP**
A PR-style view of exactly what the agent changed, built from walkback's own before-snapshots:

```diff
 src/auth.ts  modified  +2 -2
  -const KEY = "prod-secret"
  +const KEY = ""
 2 file(s) changed, +3 -2
```

### 🌐 Network calls — actually reversed · **MCP tools**
When the agent records a mutation with a **compensator** (the request that reverses it), walkback runs it:

```
agent: POST /v1/charges          → walkback_record_http  (compensator: a refund)
        walkback_compensate                → preview: "WOULD send the refund"
        walkback_compensate execute=true   → fires it, most-recent-first
```

### ✉️ Email — honest hold-and-release · **MCP tools**
No tool can recall a *delivered* email — the recipient has a copy nothing can touch. So walkback does the one honest thing that works: it **holds the email as a draft** that has gone nowhere.

```
walkback_email_stage    to=… subject=… body=…   # held, NOT sent
walkback_email_cancel                            # delete the draft → it never existed
walkback_email_release                           # ...or actually deliver it
```

**Before release:** cancel is a true unsend. **After delivery:** it's gone, and walkback says so plainly — the most it can do then is trash *your* copy. We don't pretend to reach into other people's inboxes. Works with **Gmail** (`GMAIL_ACCESS_TOKEN`) and **Outlook / Microsoft 365** (`OUTLOOK_ACCESS_TOKEN`); walkback holds no credentials of its own.

### ☁️ Cloud & databases — any tool · **MCP tools**
walkback doesn't hardcode AWS or Postgres. The agent records the **command that reverses** what it did, and walkback runs it (dry-run gated):

```
walkback_record_reversal  description="created S3 bucket assets-prod"  command="aws s3 rb s3://assets-prod --force"
walkback_record_reversal  description="ran migration 042"             command="psql $DB -f rollback_042.sql"
walkback_compensate execute=true
```

Works with **any** cloud, database, or CLI. (For DB `UPDATE`/`DELETE`, you record the inverse — walkback runs what you give it.)

## CLI

```
walkback init                      set up walkback in this directory
walkback checkpoint [label]        mark a point you can rewind to
walkback track <path>...           capture a path before the agent changes it
walkback status                    what's changed since the last checkpoint
walkback diff                      a PR-style diff of everything the agent changed
walkback rollback [checkpoint]     rewind everything since a checkpoint
walkback revert <path>             selectively undo just one file
walkback redo                      undo the last rollback
walkback watch                     snapshot + watch the filesystem (any agent)
walkback run -- <command>          snapshot, then run any command reversibly
walkback protect / unprotect       install / remove the Claude Code auto-capture hook
```

> The CLI covers **files** (automatic). The network / cloud / DB / email reversals are driven by the agent through the **MCP tools** below — because walkback can reverse files on its own, but it can't *guess* the inverse of a network call.

## MCP server

Add to your MCP client's config (e.g. `.mcp.json`):

```json
{ "mcpServers": { "walkback": { "command": "npx", "args": ["-y", "@tathagatmaitray/walkback"] } } }
```

**16 tools:** `walkback_init` · `walkback_checkpoint` · `walkback_track` · `walkback_status` · `walkback_diff` · `walkback_log` · `walkback_rollback` · `walkback_revert` · `walkback_redo` · `walkback_record_http` · `walkback_record_reversal` · `walkback_compensate` · `walkback_email_stage` · `walkback_email_release` · `walkback_email_cancel` · `walkback_email_pending`

The server ships **instructions** (auto-injected into the agent's context) telling the agent to checkpoint first and **record the inverse** of any network / cloud / DB / email action. Not using MCP? See [docs/agent-instructions.md](docs/agent-instructions.md) for the same policy as a system-prompt block.

## Architecture

A polyglot system with a real native boundary:

```
┌─────────────────────────────┐
│  TypeScript  (agent surface) │   MCP server · compensation · email · reversals
├─────────────────────────────┤   ↕ NAPI-RS (in-process, no subprocess)
│  Rust  (the engine)          │   Effect · Journal · blob store · rollback · diff
│   crates/walkback-core       │   + the standalone `walkback` CLI
└─────────────────────────────┘
```

Rust owns the part that touches your filesystem and has to be fast and trustworthy; TypeScript owns the agent-facing surface; NAPI-RS bridges them in-process.

## Why you can trust it

A universal undo is only worth anything if it's correct under pressure:

- **Crash-safe** — journal/state written write-temp-then-rename (atomic on POSIX).
- **Rollback integrity** — if any step fails, the journal is left intact and it's safe to retry; never reports success while leaving files unrestored.
- **Concurrency-safe** — an exclusive lock, so an agent and a human can't corrupt the journal.
- **Sandboxed** — refuses paths outside the project, never captures `.undo`, auto-gitignores snapshots so secrets aren't committed.

This is tested, not asserted: unit tests per property, a **property test** that runs dozens of randomized mutation sequences and asserts byte-for-byte round-trips, a **concurrency test** that hammers one journal from many threads, and Node suites that drive real HTTP/Gmail/command reversals against mock servers. The engine suite runs in CI on **Linux, macOS, and Windows**.

> **Platform note:** the engine is verified on all three OSes. On Windows, content + structure + mtime restore exactly; unix permission bits and symlink fidelity are POSIX-only (they no-op rather than fail).

## License

MIT © Tathagat Maitray
