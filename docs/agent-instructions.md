# Making walkback reliable in the wild

`walkback` provides the *mechanism* to reverse what an AI agent does. For plain file
edits that's fully automatic. For everything else — network calls, cloud resources,
database writes, email — **walkback cannot guess the inverse.** The agent has to
record it at the moment it acts.

If you use the **MCP server**, this policy is already injected into the agent's
context via the server's `instructions` — you don't have to do anything.

If you're driving the **CLI** from a custom agent, or writing your own system
prompt, paste the block below in. This is what closes the gap between "reversible"
and "lost."

> **Two surfaces:** file operations are the **CLI** (`walkback <command>`); the
> network / cloud / DB / email reversals are **MCP tools** (`walkback_*`) the agent
> calls. Recording and replaying a non-file inverse requires the MCP server — the
> CLI alone covers files.

---

## System-prompt block (copy-paste)

```
You have a "walkback" safety layer. Make your actions reversible:

1. Checkpoint first. Before a task, checkpoint (CLI: `walkback checkpoint`, or MCP:
   walkback_checkpoint). Capture files before you change them (`walkback track <path>`
   / walkback_track, directories included). File changes are then reversible with
   `walkback rollback` (or `walkback revert <file>` for one file).

2. Record the inverse of anything that isn't a file edit — walkback can't guess it.
   Use the walkback MCP tools:
   • Network mutation (POST/PUT/PATCH/DELETE): call walkback_record_http with the
     compensating request (the DELETE that undoes the POST, the refund for a charge).
   • Cloud / database change: call walkback_record_reversal with the command that
     reverses it (e.g. `aws s3 rb s3://bucket`, an inverse SQL statement). For
     UPDATE/DELETE, capture the prior values first so you can build the inverse.
   • Email: don't send directly — walkback_email_stage holds it as a draft, then
     walkback_email_release sends it or walkback_email_cancel truly unsends it.

3. Review and reverse. `walkback diff` (or walkback_diff) shows everything you changed.
   `walkback rollback` reverses files; walkback_compensate runs the recorded
   network/cloud/DB reversals (preview first, then execute=true).

Recording the inverse is part of doing the action, not an afterthought.
```

---

## Why this is necessary (the honest version)

walkback is **not** a network proxy — it doesn't intercept the agent's HTTP traffic,
so it can't automatically know that a `POST /charges` should be reversed by a
`POST /refunds`. Making that fully automatic would mean routing all of the agent's
traffic through walkback as a man-in-the-middle, which is a much larger and riskier
design. Instead, walkback keeps a clean, auditable record of inverses **that the
agent supplies** — and the instructions above are what make the agent supply them
consistently. File reversal needs none of this; it's automatic.
