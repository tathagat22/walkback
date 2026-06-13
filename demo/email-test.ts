// Proves email undo's honest guarantee against a mock Gmail API:
//   stage -> a draft is created (sent nowhere)
//   cancel -> the draft is DELETED and no send ever happens  => true unsend
//   release -> the draft is actually sent
// No real Gmail involved — we stand up a fake Gmail endpoint and watch the calls.

import { createServer } from "node:http";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const calls: Array<{ method: string; url: string }> = [];

const server = createServer((req, res) => {
  calls.push({ method: req.method ?? "", url: req.url ?? "" });
  // drain body
  req.on("data", () => {});
  req.on("end", () => {
    if (req.method === "DELETE") {
      res.statusCode = 204;
      res.end();
    } else if (req.url?.endsWith("/drafts/send")) {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ id: "msg_sent_1" }));
    } else {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ id: "draft_1" }));
    }
  });
});
await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
const port = (server.address() as { port: number }).port;

process.env.GMAIL_ACCESS_TOKEN = "test-token";
process.env.GMAIL_API_BASE = `http://127.0.0.1:${port}`;

// Import AFTER env is set (the client reads env at call time, but be explicit).
const { stageEmail, cancelEmail, releaseEmail, listPending } = await import("../src/email.js");

const dir = mkdtempSync(join(tmpdir(), "undo-email-"));
let failures = 0;
const check = (cond: boolean, msg: string) => {
  console.log(`${cond ? "✓" : "✗"} ${msg}`);
  if (!cond) failures++;
};

try {
  // 1. Stage — held as a draft, sent nowhere.
  const held = await stageEmail(dir, {
    to: "boss@example.com",
    subject: "I QUIT",
    body: "effective immediately",
  });
  check(held.draftId === "draft_1", "stage creates a Gmail draft");
  check(
    calls.some((c) => c.method === "POST" && c.url === "/users/me/drafts"),
    "a draft was created via the API",
  );
  check(listPending(dir).length === 1, "the held email is tracked as pending");
  check(
    !calls.some((c) => c.url?.endsWith("/drafts/send")),
    "nothing was sent on stage",
  );

  // 2. Cancel — true unsend: the draft is deleted and never sent.
  await cancelEmail(dir);
  check(
    calls.some((c) => c.method === "DELETE" && c.url === "/users/me/drafts/draft_1"),
    "cancel DELETEs the draft",
  );
  check(
    !calls.some((c) => c.url?.endsWith("/drafts/send")),
    "the email was NEVER sent — true unsend",
  );
  check(listPending(dir).length === 0, "no longer pending after cancel");

  // 3. Release path — actually delivers.
  await stageEmail(dir, { to: "ok@example.com", subject: "real", body: "send me" });
  await releaseEmail(dir);
  check(
    calls.some((c) => c.method === "POST" && c.url === "/users/me/drafts/send"),
    "release actually sends the draft",
  );
  check(listPending(dir).length === 0, "no longer pending after release");

  console.log(failures === 0 ? "\n✓ email undo test passed" : `\n✗ ${failures} check(s) failed`);
} finally {
  server.close();
  rmSync(dir, { recursive: true, force: true });
}

process.exit(failures === 0 ? 0 : 1);
