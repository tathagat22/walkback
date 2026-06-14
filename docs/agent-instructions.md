# Making undo reliable in the wild

`undo` provides the *mechanism* to reverse what an AI agent does. For plain file
edits that's fully automatic. For everything else — network calls, cloud
resources, database writes, email — **undo cannot guess the inverse.** The agent
has to record it at the moment it acts.

If you use the **MCP server**, this policy is already injected into the agent's
context via the server's `instructions` — you don't have to do anything.

If you **don't** use MCP (e.g. you're driving the CLI from a custom agent, or
writing your own system prompt), paste the block below into your agent's system
prompt. This is what closes the gap between "reversible" and "lost."

---

## System-prompt block (copy-paste)

```
You have an "undo" safety layer. Make your actions reversible:

1. Checkpoint first. Before a task, run `undo checkpoint`. Capture files before
   you change them with `undo track <path>` (directories included). File changes
   are then reversible with `undo rollback`, or `undo revert <file>` for one file.

2. Record the inverse of anything that isn't a file edit — undo can't guess it:
   • Network mutation (POST/PUT/PATCH/DELETE): record the compensating request
     (the DELETE that undoes the POST, the refund for a charge).
   • Cloud / database change: record the command that reverses it
     (e.g. `aws s3 rb s3://bucket`, an inverse SQL statement). For UPDATE/DELETE,
     capture the prior values first so you can build the inverse.
   • Email: don't send directly — stage it as a draft, then release or cancel.

3. Review with `undo diff`. Reverse files with `undo rollback`; run recorded
   network/cloud/DB reversals with `undo compensate` (preview first).

Recording the inverse is part of doing the action, not an afterthought.
```

---

## Why this is necessary (the honest version)

undo is **not** a network proxy — it doesn't intercept the agent's HTTP traffic,
so it can't automatically know that a `POST /charges` should be reversed by a
`POST /refunds`. Making that fully automatic would mean routing all of the
agent's traffic through undo as a man-in-the-middle, which is a much larger and
riskier design. Instead, undo keeps a clean, auditable record of inverses **that
the agent supplies** — and the instructions above are what make the agent supply
them consistently. File reversal needs none of this; it's automatic.
