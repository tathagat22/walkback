// Proves HTTP auto-compensation actually reverses a network call: we stand up a
// real local server, record a mutation whose compensator points at it, and
// assert the compensating request is (a) NOT sent on dry-run and (b) genuinely
// received on execute.

import { createServer } from "node:http";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import engine from "../src/engine.js";
import { compensate } from "../src/compensate.js";

const received: Array<{ method: string; url: string }> = [];

const server = createServer((req, res) => {
  received.push({ method: req.method ?? "", url: req.url ?? "" });
  res.statusCode = 204;
  res.end();
});
await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
const port = (server.address() as { port: number }).port;
const base = `http://127.0.0.1:${port}`;

const dir = mkdtempSync(join(tmpdir(), "undo-comp-"));
let failures = 0;
const check = (cond: boolean, msg: string) => {
  console.log(`${cond ? "✓" : "✗"} ${msg}`);
  if (!cond) failures++;
};

try {
  engine.init(dir);
  engine.checkpoint(dir, "before agent");

  // Agent makes a POST and records how to reverse it: DELETE the created charge.
  engine.recordHttp(
    dir,
    "POST",
    "https://api.example.com/v1/charges",
    "DELETE",
    `${base}/v1/charges/ch_123`,
    null,
  );

  // 1. Dry run must NOT touch the network.
  const dry = await compensate(dir, false);
  check(received.length === 0, "dry run sends nothing");
  check(dry.actions[0]?.status === "planned", "dry run reports the planned compensator");

  // 2. Execute must actually fire the compensating request.
  const done = await compensate(dir, true);
  check(received.length === 1, "execute sends exactly one request");
  check(
    received[0]?.method === "DELETE" && received[0]?.url === "/v1/charges/ch_123",
    "the server received DELETE /v1/charges/ch_123",
  );
  check(done.actions[0]?.status === "ok", "execute reports success");

  console.log(failures === 0 ? "\n✓ HTTP compensation test passed" : `\n✗ ${failures} check(s) failed`);
} finally {
  server.close();
  rmSync(dir, { recursive: true, force: true });
}

process.exit(failures === 0 ? 0 : 1);
