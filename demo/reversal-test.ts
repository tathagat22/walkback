// Proves command reversals (cloud teardown / DB inverse SQL) run only on
// execute, never on dry-run. The "reversal command" here just creates a marker
// file via node (portable on every CI runner) so we can observe whether it ran.

import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { recordReversal, listReversals } from "../src/reversals.js";
import { compensate } from "../src/compensate.js";

const dir = mkdtempSync(join(tmpdir(), "undo-rev-"));
const marker = join(dir, "reversed.txt");
let failures = 0;
const check = (cond: boolean, msg: string) => {
  console.log(`${cond ? "✓" : "✗"} ${msg}`);
  if (!cond) failures++;
};

try {
  // Stand in for "agent created a cloud resource"; record how to tear it down.
  recordReversal(dir, {
    description: "created S3 bucket assets-prod",
    command: `node -e "require('fs').writeFileSync('reversed.txt','torn-down')"`,
    cwd: dir,
  });
  check(listReversals(dir).length === 1, "reversal recorded");

  // Dry run must not run the command.
  const dry = await compensate(dir, false);
  check(!existsSync(marker), "dry run does NOT run the teardown");
  check(dry.actions.some((a) => a.status === "planned"), "dry run reports the planned teardown");

  // Execute runs it and clears it.
  const done = await compensate(dir, true);
  check(existsSync(marker), "execute runs the teardown command");
  check(done.actions.some((a) => a.status === "ok"), "execute reports success");
  check(listReversals(dir).length === 0, "executed reversal is cleared");

  console.log(failures === 0 ? "\n✓ command reversal test passed" : `\n✗ ${failures} check(s) failed`);
} finally {
  rmSync(dir, { recursive: true, force: true });
}

process.exit(failures === 0 ? 0 : 1);
