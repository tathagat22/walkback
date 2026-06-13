// Loads the NAPI-built native engine (Rust `undo-core`) and gives it a typed
// TypeScript face. Everything above this line is JS/TS; everything the engine
// actually does happens in Rust, in-process.

import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

export interface UndoEngine {
  /** Create a `.undo` directory under `workdir`. */
  init(workdir: string): void;
  /** Mark a checkpoint. Returns its id. */
  checkpoint(workdir: string, label: string): string;
  /** Capture a file before the agent changes it. Returns a description. */
  track(workdir: string, path: string): string;
  /** Record a network mutation with an optional compensating request. */
  recordHttp(
    workdir: string,
    method: string,
    url: string,
    compMethod?: string | null,
    compUrl?: string | null,
    compBody?: string | null,
  ): void;
  /** JSON `{ checkpoint, effects }` since the last checkpoint. */
  statusJson(workdir: string): string;
  /** JSON array of every journal row, oldest first. */
  logJson(workdir: string): string;
  /** Rewind everything since a checkpoint. Returns a JSON report. */
  rollback(workdir: string, target?: string | null): string;
  /** Undo the last rollback. Returns a JSON `{ restored, failed }` report. */
  redo(workdir: string): string;
  /** Selective undo: reverse just one file. Returns a description, or null. */
  revert(workdir: string, path: string): string | null;
  /** JSON `[{ path, status, added, removed, hunk }]` — diff since the checkpoint. */
  diffJson(workdir: string): string;
}

const engine = require("@agent-undo/engine") as UndoEngine;

export default engine;
