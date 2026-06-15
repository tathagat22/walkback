// Boots the real MCP server as a subprocess and drives it through a full
// scenario over stdio, exactly as an agent (Claude Code etc.) would.

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { mkdtempSync, writeFileSync, readFileSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const sandbox = mkdtempSync(join(tmpdir(), "undo-mcp-"));

const transport = new StdioClientTransport({
  command: "npx",
  args: ["tsx", join(root, "src/mcp.ts")],
});
const client = new Client({ name: "smoke-test", version: "0.0.0" });
await client.connect(transport);

const call = async (name: string, args: Record<string, unknown>) => {
  const res: any = await client.callTool({ name, arguments: { cwd: sandbox, ...args } });
  return res.content[0].text as string;
};

const tools = await client.listTools();
console.log("tools exposed:", tools.tools.map((t) => t.name).join(", "));
console.log();

console.log(await call("walkback_init", {}));
console.log(await call("walkback_checkpoint", { label: "before the agent runs" }));

// Agent is about to touch two files.
const cfg = join(sandbox, "config.json");
writeFileSync(cfg, '{"apiKey":"keep-me"}');
console.log(await call("walkback_track", { paths: ["config.json", "feature.ts"] }));

// Agent acts (and goes wrong).
writeFileSync(cfg, '{"apiKey":""}');
writeFileSync(join(sandbox, "feature.ts"), "// broken");
await call("walkback_record_http", {
  method: "POST",
  url: "https://api.example.com/charges",
  compensatorMethod: "DELETE",
  compensatorUrl: "https://api.example.com/charges/ch_123",
});

console.log("\n" + (await call("walkback_status", {})));

console.log("\nconfig before rollback:", readFileSync(cfg, "utf8"));
console.log("\n" + (await call("walkback_rollback", {})));
console.log("\nconfig after rollback: ", readFileSync(cfg, "utf8"));
console.log("feature.ts exists after rollback:", existsSync(join(sandbox, "feature.ts")));

await client.close();
rmSync(sandbox, { recursive: true, force: true });

const ok = readFileSync; // noop to keep import used
void ok;
console.log("\n✓ MCP smoke test complete");
