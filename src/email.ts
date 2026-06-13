// Email undo — built on Gmail hold-and-release.
//
// stage  → the agent's email becomes a held draft (sent nowhere yet)
// release→ deliver it (after this it's gone — no recall)
// cancel → delete the held draft → it never existed for the recipient
//
// Held drafts are tracked in `.undo/pending-emails.json` so you can list and
// cancel them later. `undo` carries no Google credentials itself — it uses a
// bearer token from the GMAIL_ACCESS_TOKEN env var.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { GmailClient, type EmailDraft } from "./providers/gmail.js";

export interface PendingEmail {
  draftId: string;
  to: string;
  subject: string;
  stagedAt: string;
}

function storePath(cwd: string): string {
  return join(cwd, ".undo", "pending-emails.json");
}

function loadPending(cwd: string): PendingEmail[] {
  try {
    return JSON.parse(readFileSync(storePath(cwd), "utf8")) as PendingEmail[];
  } catch {
    return [];
  }
}

function savePending(cwd: string, list: PendingEmail[]): void {
  const dir = join(cwd, ".undo");
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
  writeFileSync(storePath(cwd), JSON.stringify(list, null, 2) + "\n");
}

function gmail(): GmailClient {
  const token = process.env.GMAIL_ACCESS_TOKEN;
  if (!token) {
    throw new Error("email undo needs a Gmail token — set GMAIL_ACCESS_TOKEN");
  }
  const base = process.env.GMAIL_API_BASE ?? "https://gmail.googleapis.com/gmail/v1";
  return new GmailClient(base, token);
}

/** Hold an email as a draft. It is NOT sent until released. */
export async function stageEmail(cwd: string, draft: EmailDraft): Promise<PendingEmail> {
  const { id } = await gmail().createDraft(draft);
  const entry: PendingEmail = {
    draftId: id,
    to: draft.to,
    subject: draft.subject,
    stagedAt: new Date().toISOString(),
  };
  savePending(cwd, [...loadPending(cwd), entry]);
  return entry;
}

export function listPending(cwd: string): PendingEmail[] {
  return loadPending(cwd);
}

function select(cwd: string, draftId?: string): PendingEmail[] {
  const all = loadPending(cwd);
  return draftId ? all.filter((e) => e.draftId === draftId) : all;
}

/** Deliver held emails (all, or one by id). After this they're gone — no recall. */
export async function releaseEmail(
  cwd: string,
  draftId?: string,
): Promise<PendingEmail[]> {
  const targets = select(cwd, draftId);
  const client = gmail();
  for (const e of targets) await client.sendDraft(e.draftId);
  const sentIds = new Set(targets.map((e) => e.draftId));
  savePending(cwd, loadPending(cwd).filter((e) => !sentIds.has(e.draftId)));
  return targets;
}

/** Cancel held emails (all, or one by id) — true unsend; they never go out. */
export async function cancelEmail(
  cwd: string,
  draftId?: string,
): Promise<PendingEmail[]> {
  const targets = select(cwd, draftId);
  const client = gmail();
  for (const e of targets) await client.deleteDraft(e.draftId);
  const cancelledIds = new Set(targets.map((e) => e.draftId));
  savePending(cwd, loadPending(cwd).filter((e) => !cancelledIds.has(e.draftId)));
  return targets;
}
