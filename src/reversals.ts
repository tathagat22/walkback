// Command reversals — the universal mechanism behind cloud-resource teardown
// and database rollback.
//
// undo does not need to understand AWS, GCP, Terraform, Postgres, or MySQL. When
// an agent does something with an external system, it records the *command that
// reverses it* — `terraform destroy`, `aws s3 rb s3://bucket`, an inverse SQL via
// `psql -c "..."`. undo runs that command when you compensate, gated behind a
// dry-run so it never fires blindly. This works with ANY tool, not a hardcoded few.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { randomUUID } from "node:crypto";

export interface Reversal {
  id: string;
  /** What the agent did, e.g. "created S3 bucket assets-prod". */
  description: string;
  /** The command that reverses it, e.g. "aws s3 rb s3://assets-prod --force". */
  command: string;
  /** Where to run it (defaults to the project dir). */
  cwd?: string;
  recordedAt: string;
}

function storePath(workdir: string): string {
  return join(workdir, ".undo", "reversals.json");
}

export function listReversals(workdir: string): Reversal[] {
  try {
    return JSON.parse(readFileSync(storePath(workdir), "utf8")) as Reversal[];
  } catch {
    return [];
  }
}

function writeReversals(workdir: string, list: Reversal[]): void {
  const dir = join(workdir, ".undo");
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
  writeFileSync(storePath(workdir), JSON.stringify(list, null, 2) + "\n");
}

export function recordReversal(
  workdir: string,
  r: { description: string; command: string; cwd?: string },
): Reversal {
  const entry: Reversal = { id: randomUUID(), recordedAt: new Date().toISOString(), ...r };
  writeReversals(workdir, [...listReversals(workdir), entry]);
  return entry;
}

/** Drop reversals (by id) that have been executed. */
export function clearReversals(workdir: string, ids: string[]): void {
  const done = new Set(ids);
  writeReversals(
    workdir,
    listReversals(workdir).filter((r) => !done.has(r.id)),
  );
}
