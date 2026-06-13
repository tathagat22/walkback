// HTTP auto-compensation — the moat.
//
// The engine records every network mutation an agent makes together with a
// *compensator*: the request that reverses it (a DELETE that undoes a POST, a
// refund that undoes a charge). This turns those recorded compensators into
// real, executed reversals — the part git, and every file-only "undo", cannot do.
//
// Safety model: dry-run by default. Nothing hits the network unless the caller
// explicitly passes `execute: true`. Network undo is powerful and one-way, so it
// is never silent.

import { exec } from "node:child_process";
import { promisify } from "node:util";
import engine from "./engine.js";
import { listReversals, clearReversals } from "./reversals.js";

const run = promisify(exec);

export type ActionStatus = "planned" | "ok" | "failed" | "no-compensator";

export interface CompensateAction {
  /** The original mutation, e.g. "POST https://api.stripe.com/v1/charges". */
  original: string;
  /** The reversing request, e.g. "POST https://api.stripe.com/v1/refunds", or null. */
  compensator: string | null;
  status: ActionStatus;
  /** HTTP status code (on execute) or an error message. */
  detail?: string;
}

export interface CompensateResult {
  executed: boolean;
  actions: CompensateAction[];
}

interface Compensator {
  method: string;
  url: string;
  body?: string | null;
}

/**
 * Reverse the network mutations recorded since the last checkpoint.
 *
 * @param workdir project directory (where `.undo` lives)
 * @param execute when false (default) this only previews; when true it fires the
 *                compensating requests in reverse order (most recent first)
 */
export async function compensate(
  workdir: string,
  execute = false,
): Promise<CompensateResult> {
  const status = JSON.parse(engine.statusJson(workdir));
  const effects: Array<Record<string, unknown>> = status.effects ?? [];

  // Most-recent-first, like a rollback.
  const mutations = effects
    .filter((e) => e.kind === "http_mutation")
    .reverse();

  const actions: CompensateAction[] = [];
  for (const e of mutations) {
    const original = `${e.method} ${e.url}`;
    const comp = e.compensator as Compensator | null | undefined;

    if (!comp) {
      actions.push({ original, compensator: null, status: "no-compensator" });
      continue;
    }
    const compensator = `${comp.method} ${comp.url}`;

    if (!execute) {
      actions.push({ original, compensator, status: "planned" });
      continue;
    }

    try {
      const res = await fetch(comp.url, {
        method: comp.method,
        body: comp.body ?? undefined,
        headers: comp.body ? { "content-type": "application/json" } : undefined,
      });
      actions.push({
        original,
        compensator,
        status: res.ok ? "ok" : "failed",
        detail: `HTTP ${res.status}`,
      });
    } catch (err) {
      actions.push({
        original,
        compensator,
        status: "failed",
        detail: err instanceof Error ? err.message : String(err),
      });
    }
  }

  // Command reversals (cloud teardown, DB inverse SQL, anything scriptable),
  // also most-recent-first. Gated by the same execute flag.
  const reversals = listReversals(workdir).slice().reverse();
  const executedIds: string[] = [];
  for (const r of reversals) {
    const compensator = `$ ${r.command}`;
    if (!execute) {
      actions.push({ original: r.description, compensator, status: "planned" });
      continue;
    }
    try {
      await run(r.command, { cwd: r.cwd ?? workdir });
      actions.push({ original: r.description, compensator, status: "ok", detail: "exit 0" });
      executedIds.push(r.id);
    } catch (err) {
      actions.push({
        original: r.description,
        compensator,
        status: "failed",
        detail: err instanceof Error ? err.message.split("\n")[0] : String(err),
      });
    }
  }
  if (executedIds.length) clearReversals(workdir, executedIds);

  return { executed: execute, actions };
}

/** Render a compensation result as a human-readable block. */
export function renderCompensateResult(r: CompensateResult): string {
  if (r.actions.length === 0) return "No network mutations recorded to compensate.";
  const header = r.executed
    ? "Executed compensating requests (most recent first):"
    : "Dry run — these compensating requests WOULD be sent (pass execute=true to fire them):";
  const lines = r.actions.map((a) => {
    switch (a.status) {
      case "planned":
        return `  • ${a.compensator}   ⟵ reverses ${a.original}`;
      case "ok":
        return `  ✓ ${a.compensator}   (${a.detail})   ⟵ reversed ${a.original}`;
      case "failed":
        return `  ✗ ${a.compensator}   (${a.detail})   ⟵ for ${a.original}`;
      case "no-compensator":
        return `  ? ${a.original}   (no compensator recorded — manual)`;
    }
  });
  return [header, ...lines].join("\n");
}
