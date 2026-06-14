#!/usr/bin/env node
// The `undo` MCP server — Undo anything your AI agent does.
//
// An agent checkpoints itself before acting, tracks each path it's about to
// change (files or whole directories), and records network mutations. If
// anything goes wrong, the human (or the agent) calls undo_rollback and the
// world snaps back — and undo_redo puts it back if they change their mind.
//
// Every handler is wrapped so an engine error becomes a structured MCP error
// instead of crashing the server.

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import engine from "./engine.js";
import { compensate, renderCompensateResult } from "./compensate.js";
import { stageEmail, releaseEmail, cancelEmail, listPending } from "./email.js";
import { recordReversal } from "./reversals.js";

// Injected into the agent's context by MCP clients. This is what makes the
// reversibility actually reliable in the wild: undo provides the mechanism, but
// the agent has to record the inverse of anything that isn't a plain file edit.
const INSTRUCTIONS = `This server makes your actions reversible. Follow this policy:

1. CHECKPOINT FIRST. Before you start a task, call undo_checkpoint once. Before
   editing/creating/deleting a file, call undo_track on it (directories work too).
   File changes are then fully reversible with undo_rollback (or undo_revert for one file).

2. RECORD THE INVERSE OF EVERYTHING ELSE — undo cannot guess it:
   • Network mutation (POST/PUT/PATCH/DELETE): immediately call undo_record_http with
     the COMPENSATING request (e.g. the DELETE that removes what you just POSTed, the
     refund for a charge). Without this, the call cannot be reversed.
   • Cloud or database change: call undo_record_reversal with the exact command that
     reverses it (e.g. "aws s3 rb s3://bucket", an inverse SQL statement). For UPDATE/
     DELETE, capture the prior values first so you can build the inverse.
   • Email: do NOT send directly. Call undo_email_stage to hold it as a draft, then
     undo_email_release to send or undo_email_cancel to truly unsend it.

3. REVIEW + REVERSE. undo_diff shows everything you changed. undo_rollback reverses files;
   undo_compensate (dry-run first, then execute=true) runs the recorded network/cloud/DB
   reversals; undo_redo re-applies a rollback.

Recording the inverse at the moment you act is the difference between "reversible" and
"lost". Treat it as part of doing the action, not an afterthought.`;

const server = new McpServer(
  { name: "walkback", version: "0.2.1" },
  { instructions: INSTRUCTIONS },
);

const cwdSchema = {
  cwd: z
    .string()
    .optional()
    .describe("Project directory. Defaults to the server's working directory."),
};
const wd = (cwd?: string) => cwd ?? process.cwd();
const ok = (text: string) => ({ content: [{ type: "text" as const, text }] });
const fail = (e: unknown) => ({
  content: [
    {
      type: "text" as const,
      text: `undo error: ${e instanceof Error ? e.message : String(e)}`,
    },
  ],
  isError: true,
});

// Wrap a handler so any thrown engine error is returned as a structured MCP
// error rather than taking down the server process.
const guard =
  <A>(fn: (args: A) => string) =>
  async (args: A) => {
    try {
      return ok(fn(args));
    } catch (e) {
      return fail(e);
    }
  };

server.registerTool(
  "undo_init",
  {
    title: "Initialize undo",
    description:
      "Set up the undo time machine in a project directory (and gitignore its snapshots). Run once before checkpointing.",
    inputSchema: cwdSchema,
  },
  guard(({ cwd }) => {
    engine.init(wd(cwd));
    return `Initialized undo in ${wd(cwd)}/.undo (added .undo/ to .gitignore)`;
  }),
);

server.registerTool(
  "undo_checkpoint",
  {
    title: "Create a checkpoint",
    description:
      "Mark a point in time you can rewind to. Call this BEFORE you start making changes.",
    inputSchema: {
      ...cwdSchema,
      label: z.string().describe("A short description, e.g. 'before refactor'."),
    },
  },
  guard(({ cwd, label }) => {
    const id = engine.checkpoint(wd(cwd), label);
    return `Checkpoint ${id} created: "${label}"`;
  }),
);

server.registerTool(
  "undo_track",
  {
    title: "Track a path before changing it",
    description:
      "Capture a file's (or whole directory's) current state BEFORE you create, modify, or delete it. " +
      "This is what makes the change reversible. Call it on every path you're about to touch. " +
      "Directories are captured recursively. Paths outside the project are refused.",
    inputSchema: {
      ...cwdSchema,
      paths: z
        .array(z.string())
        .describe("Files or directories you're about to change (relative or absolute)."),
    },
  },
  guard(({ cwd, paths }) => {
    const lines = paths.map((p) => "  " + engine.track(wd(cwd), p).replace(/\n/g, "\n  "));
    return `Tracking ${paths.length} path(s):\n${lines.join("\n")}`;
  }),
);

server.registerTool(
  "undo_record_http",
  {
    title: "Record a network mutation",
    description:
      "Log a POST/PUT/PATCH/DELETE the agent made, with an optional compensating request " +
      "(e.g. a DELETE that reverses a POST) so it can be undone later.",
    inputSchema: {
      ...cwdSchema,
      method: z.string().describe("HTTP method of the mutation."),
      url: z.string().describe("URL that was called."),
      compensatorMethod: z.string().optional().describe("Method of the reversing request."),
      compensatorUrl: z.string().optional().describe("URL of the reversing request."),
      compensatorBody: z.string().optional().describe("Body of the reversing request."),
    },
  },
  guard(({ cwd, method, url, compensatorMethod, compensatorUrl, compensatorBody }) => {
    engine.recordHttp(
      wd(cwd),
      method,
      url,
      compensatorMethod ?? null,
      compensatorUrl ?? null,
      compensatorBody ?? null,
    );
    return `Recorded ${method} ${url}`;
  }),
);

server.registerTool(
  "undo_status",
  {
    title: "What's changed since the checkpoint",
    description: "Show every effect recorded since the most recent checkpoint.",
    inputSchema: cwdSchema,
  },
  guard(({ cwd }) => {
    const status = JSON.parse(engine.statusJson(wd(cwd)));
    if (!status.checkpoint) return "No checkpoint yet. Call undo_checkpoint first.";
    const [id, label] = status.checkpoint;
    const effects: string[] = (status.effects ?? []).map(describeEffect);
    if (effects.length === 0) return `On checkpoint ${id} ("${label}"). Nothing recorded yet.`;
    return (
      `On checkpoint ${id} ("${label}"). ${effects.length} change(s):\n` +
      effects.map((e) => "  " + e).join("\n")
    );
  }),
);

server.registerTool(
  "undo_log",
  {
    title: "Full undo history",
    description: "List every checkpoint and effect in order.",
    inputSchema: cwdSchema,
  },
  guard(({ cwd }) => {
    const rows = JSON.parse(engine.logJson(wd(cwd))) as any[];
    if (rows.length === 0) return "History is empty.";
    return rows
      .map((r) =>
        r.type === "checkpoint" ? `● ${r.id}  "${r.label}"` : "    " + describeEffect(r.effect),
      )
      .join("\n");
  }),
);

server.registerTool(
  "undo_rollback",
  {
    title: "Rewind everything",
    description:
      "Reverse every change made since a checkpoint (the latest one by default). " +
      "Files, directories, and symlinks are restored exactly; network/shell effects are listed " +
      "for manual handling. If any step fails, the journal is left intact so you can safely retry. " +
      "Use undo_redo to reverse a rollback.",
    inputSchema: {
      ...cwdSchema,
      checkpoint: z
        .string()
        .optional()
        .describe("Checkpoint id to rewind to. Defaults to the most recent."),
    },
  },
  guard(({ cwd, checkpoint }) => {
    const r = JSON.parse(engine.rollback(wd(cwd), checkpoint ?? null));
    const lines: string[] = [];
    if (r.failed?.length) {
      lines.push(`Rollback to ${r.checkpoint} INCOMPLETE — journal left intact, safe to retry.`);
    } else {
      lines.push(`Rewound to ${r.checkpoint}.`);
    }
    if (r.reverted?.length) lines.push("Reverted:", ...r.reverted.map((x: string) => "  ✓ " + x));
    if (r.skipped?.length) lines.push("Manual:", ...r.skipped.map((x: string) => "  • " + x));
    if (r.failed?.length) lines.push("Failed:", ...r.failed.map((x: string) => "  ✗ " + x));
    if (!r.reverted?.length && !r.skipped?.length && !r.failed?.length)
      lines.push("Nothing to undo.");
    return lines.join("\n");
  }),
);

server.registerTool(
  "undo_redo",
  {
    title: "Undo the last rollback",
    description:
      "Re-apply the changes that the most recent undo_rollback reversed, and re-extend the " +
      "history so you can roll back again.",
    inputSchema: cwdSchema,
  },
  guard(({ cwd }) => {
    const r = JSON.parse(engine.redo(wd(cwd)));
    const lines = [r.failed?.length ? "Redo INCOMPLETE." : "Redid the last rollback."];
    if (r.restored?.length) lines.push(...r.restored.map((x: string) => "  ✓ " + x));
    if (r.failed?.length) lines.push("Failed:", ...r.failed.map((x: string) => "  ✗ " + x));
    return lines.join("\n");
  }),
);

server.registerTool(
  "undo_diff",
  {
    title: "Review what the agent changed",
    description:
      "A PR-style diff of every file changed since the last checkpoint — the reviewable " +
      "'here's exactly what I did' surface. Built from undo's own before-snapshots.",
    inputSchema: cwdSchema,
  },
  guard(({ cwd }) => {
    const entries = JSON.parse(engine.diffJson(wd(cwd))) as Array<{
      path: string;
      status: string;
      added: number;
      removed: number;
      hunk: string;
    }>;
    if (entries.length === 0) return "No changes since the checkpoint.";
    let added = 0;
    let removed = 0;
    const blocks = entries.map((e) => {
      added += e.added;
      removed += e.removed;
      return `${e.path}  [${e.status}]  +${e.added} -${e.removed}\n${e.hunk}`.trimEnd();
    });
    return (
      blocks.join("\n\n") +
      `\n\n${entries.length} file(s) changed, +${added} -${removed}`
    );
  }),
);

server.registerTool(
  "undo_revert",
  {
    title: "Selectively undo one file",
    description:
      "Reverse just a single file (the most recent change to it), leaving every other change since " +
      "the checkpoint in place. The opposite of rollback's all-or-nothing.",
    inputSchema: {
      ...cwdSchema,
      path: z.string().describe("The file to revert."),
    },
  },
  guard(({ cwd, path }) => {
    const msg = engine.revert(wd(cwd), path);
    return msg ?? `${path} was not tracked — nothing to revert.`;
  }),
);

server.registerTool(
  "undo_record_reversal",
  {
    title: "Record how to reverse a cloud/DB action",
    description:
      "Record the command that reverses something the agent did to an external system — a cloud " +
      "resource (e.g. 'terraform destroy', 'aws s3 rb s3://bucket') or a database change (e.g. an " +
      "inverse SQL via psql). undo_compensate will run it (dry-run gated). Works with any tool. " +
      "For UPDATE/DELETE you must capture the prior values to build the inverse — undo runs what you give it.",
    inputSchema: {
      ...cwdSchema,
      description: z.string().describe("What was done, e.g. 'created S3 bucket assets-prod'."),
      command: z.string().describe("The command that reverses it, e.g. 'aws s3 rb s3://assets-prod'."),
      runIn: z.string().optional().describe("Directory to run the command in. Defaults to the project."),
    },
  },
  guard(({ cwd, description, command, runIn }) => {
    recordReversal(wd(cwd), { description, command, cwd: runIn });
    return `Recorded reversal for "${description}":\n  $ ${command}\n  run it with undo_compensate (execute=true)`;
  }),
);

server.registerTool(
  "undo_compensate",
  {
    title: "Reverse network + cloud/DB actions",
    description:
      "Execute the compensating actions recorded since the last checkpoint: the reversing HTTP " +
      "request (DELETE undoes POST, refund undoes charge) AND any recorded reversal commands " +
      "(cloud teardown, inverse SQL). Dry-run by default — pass execute=true to fire them. Most-recent-first.",
    inputSchema: {
      ...cwdSchema,
      execute: z
        .boolean()
        .optional()
        .describe("If true, actually send the compensating requests. Defaults to false (preview)."),
    },
  },
  async ({ cwd, execute }) => {
    try {
      const result = await compensate(wd(cwd), execute ?? false);
      return ok(renderCompensateResult(result));
    } catch (e) {
      return fail(e);
    }
  },
);

server.registerTool(
  "undo_email_stage",
  {
    title: "Send an email — reversibly (hold as draft)",
    description:
      "Hold an email instead of sending it immediately: it becomes a Gmail DRAFT that has gone " +
      "nowhere. Release it with undo_email_release to actually deliver, or undo_email_cancel to " +
      "truly unsend it (it never reaches the recipient). Needs GMAIL_ACCESS_TOKEN.",
    inputSchema: {
      ...cwdSchema,
      to: z.string().describe("Recipient email address."),
      subject: z.string().describe("Subject line."),
      body: z.string().describe("Plain-text body."),
    },
  },
  async ({ cwd, to, subject, body }) => {
    try {
      const e = await stageEmail(wd(cwd), { to, subject, body });
      return ok(
        `Held (not sent): "${e.subject}" → ${e.to}  [draft ${e.draftId}]\n` +
          `  release:  undo_email_release\n  unsend:   undo_email_cancel`,
      );
    } catch (e) {
      return fail(e);
    }
  },
);

server.registerTool(
  "undo_email_release",
  {
    title: "Deliver held email(s)",
    description:
      "Actually send the held draft(s). After this the email is delivered and CANNOT be recalled.",
    inputSchema: {
      ...cwdSchema,
      draftId: z.string().optional().describe("A specific held draft id, or omit for all."),
    },
  },
  async ({ cwd, draftId }) => {
    try {
      const sent = await releaseEmail(wd(cwd), draftId);
      if (sent.length === 0) return ok("Nothing held to release.");
      return ok(`Delivered ${sent.length} email(s): ${sent.map((e) => e.to).join(", ")}`);
    } catch (e) {
      return fail(e);
    }
  },
);

server.registerTool(
  "undo_email_cancel",
  {
    title: "Unsend held email(s)",
    description:
      "Delete the held draft(s) so they never go out — a true unsend, possible only because they " +
      "were never delivered. Does nothing to emails already released.",
    inputSchema: {
      ...cwdSchema,
      draftId: z.string().optional().describe("A specific held draft id, or omit for all."),
    },
  },
  async ({ cwd, draftId }) => {
    try {
      const cancelled = await cancelEmail(wd(cwd), draftId);
      if (cancelled.length === 0) return ok("Nothing held to cancel.");
      return ok(
        `Unsent ${cancelled.length} email(s) — never delivered: ${cancelled.map((e) => e.subject).join("; ")}`,
      );
    } catch (e) {
      return fail(e);
    }
  },
);

server.registerTool(
  "undo_email_pending",
  {
    title: "List held emails",
    description: "Show emails staged but not yet released — these can still be cancelled.",
    inputSchema: cwdSchema,
  },
  guard(({ cwd }) => {
    const pending = listPending(wd(cwd));
    if (pending.length === 0) return "No held emails.";
    return pending.map((e) => `  • "${e.subject}" → ${e.to}  [draft ${e.draftId}]`).join("\n");
  }),
);

function describeEffect(e: any): string {
  switch (e.kind) {
    case "path_create":
      return `created  ${e.path}`;
    case "file":
      return `captured ${e.path}`;
    case "symlink":
      return `symlink  ${e.path}`;
    case "dir":
      return `dir      ${e.path}`;
    case "http_mutation":
      return `${e.method} ${e.url}`;
    case "exec":
      return `ran      ${e.command}`;
    default:
      return JSON.stringify(e);
  }
}

const transport = new StdioServerTransport();
await server.connect(transport);
console.error("undo MCP server running on stdio");
