// Proves email undo's honest guarantee for BOTH providers (Gmail + Outlook),
// against a mock server: stage creates a draft (sent nowhere), cancel deletes it
// and nothing is ever sent (true unsend), release actually sends.

import { createServer } from "node:http";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { stageEmail, cancelEmail, releaseEmail, listPending } from "../src/email.js";

let calls: Array<{ method: string; url: string }> = [];
const server = createServer((req, res) => {
  calls.push({ method: req.method ?? "", url: req.url ?? "" });
  req.on("data", () => {});
  req.on("end", () => {
    if (req.method === "DELETE") {
      res.statusCode = 204;
      res.end();
    } else if (req.url?.endsWith("/send")) {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ id: "sent_1" }));
    } else {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ id: "draft_1" }));
    }
  });
});
await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
const port = (server.address() as { port: number }).port;
const base = `http://127.0.0.1:${port}`;

let failures = 0;
const check = (cond: boolean, msg: string) => {
  console.log(`${cond ? "✓" : "✗"} ${msg}`);
  if (!cond) failures++;
};

async function flow(label: string, env: Record<string, string>, createUrl: string, sendUrl: string) {
  // reset env for a clean provider selection
  for (const k of ["EMAIL_PROVIDER", "GMAIL_ACCESS_TOKEN", "GMAIL_API_BASE", "OUTLOOK_ACCESS_TOKEN", "OUTLOOK_API_BASE"])
    delete process.env[k];
  Object.assign(process.env, env);
  calls = [];
  const dir = mkdtempSync(join(tmpdir(), "undo-email-"));
  try {
    await stageEmail(dir, { to: "boss@example.com", subject: "I QUIT", body: "bye" });
    check(calls.some((c) => c.method === "POST" && c.url === createUrl), `${label}: stage creates a draft (${createUrl})`);
    check(listPending(dir).length === 1, `${label}: held email tracked as pending`);
    check(!calls.some((c) => c.url?.endsWith("/send")), `${label}: nothing sent on stage`);

    await cancelEmail(dir);
    check(calls.some((c) => c.method === "DELETE"), `${label}: cancel deletes the draft`);
    check(!calls.some((c) => c.url?.endsWith("/send")), `${label}: NEVER sent — true unsend`);
    check(listPending(dir).length === 0, `${label}: not pending after cancel`);

    await stageEmail(dir, { to: "ok@example.com", subject: "real", body: "send" });
    await releaseEmail(dir);
    check(calls.some((c) => c.method === "POST" && c.url === sendUrl), `${label}: release actually sends (${sendUrl})`);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

try {
  await flow("gmail", { GMAIL_ACCESS_TOKEN: "t", GMAIL_API_BASE: base }, "/users/me/drafts", "/users/me/drafts/send");
  await flow(
    "outlook",
    { EMAIL_PROVIDER: "outlook", OUTLOOK_ACCESS_TOKEN: "t", OUTLOOK_API_BASE: base },
    "/me/messages",
    "/me/messages/draft_1/send",
  );
  console.log(failures === 0 ? "\n✓ email undo test passed (Gmail + Outlook)" : `\n✗ ${failures} check(s) failed`);
} finally {
  server.close();
}

process.exit(failures === 0 ? 0 : 1);
