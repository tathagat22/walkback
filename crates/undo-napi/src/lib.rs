//! NAPI bridge: the Rust `undo-core` engine, callable in-process from
//! TypeScript. The MCP server and any Node tooling drive the engine through
//! these functions — no subprocess, no IPC. Structured results cross the
//! boundary as JSON strings to keep the ABI small and stable.

use napi_derive::napi;
use std::path::Path;
use undo_core::{Effect, HttpCompensator, Undo};

fn err<E: std::fmt::Display>(e: E) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

fn open(workdir: &str) -> napi::Result<Undo> {
    Undo::discover(Path::new(workdir))
        .map_err(err)?
        .ok_or_else(|| napi::Error::from_reason("no .undo found here — call init() first"))
}

/// Create a `.undo` directory under `workdir`.
#[napi]
pub fn init(workdir: String) -> napi::Result<()> {
    Undo::init(Path::new(&workdir)).map(|_| ()).map_err(err)
}

/// Mark a checkpoint. Returns its id.
#[napi]
pub fn checkpoint(workdir: String, label: String) -> napi::Result<String> {
    open(&workdir)?.checkpoint(&label).map_err(err)
}

/// Capture a path (recursively, if a directory) before the agent changes it.
/// Returns a newline-joined description of every effect recorded.
#[napi]
pub fn track(workdir: String, path: String) -> napi::Result<String> {
    let effects = open(&workdir)?.track(Path::new(&path)).map_err(err)?;
    if effects.is_empty() {
        return Ok(format!("{path} (already tracked)"));
    }
    Ok(effects
        .iter()
        .map(|e| e.describe())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Record a network mutation with an optional compensating request.
#[napi]
pub fn record_http(
    workdir: String,
    method: String,
    url: String,
    comp_method: Option<String>,
    comp_url: Option<String>,
    comp_body: Option<String>,
) -> napi::Result<()> {
    let compensator = match (comp_method, comp_url) {
        (Some(m), Some(u)) => Some(HttpCompensator {
            method: m,
            url: u,
            body: comp_body,
        }),
        _ => None,
    };
    open(&workdir)?
        .record(
            Effect::HttpMutation {
                method,
                url,
                compensator,
            },
            Some("agent".to_string()),
        )
        .map_err(err)
}

/// JSON `{ checkpoint, effects }` for what's changed since the last checkpoint.
#[napi]
pub fn status_json(workdir: String) -> napi::Result<String> {
    let status = open(&workdir)?.status().map_err(err)?;
    serde_json::to_string(&status).map_err(err)
}

/// JSON array of every journal row, oldest first.
#[napi]
pub fn log_json(workdir: String) -> napi::Result<String> {
    let rows = open(&workdir)?.log().map_err(err)?;
    serde_json::to_string(&rows).map_err(err)
}

/// Rewind everything since `target` (or the latest checkpoint). Returns a JSON
/// `{ checkpoint, reverted, skipped, failed }` report. If `failed` is non-empty
/// the journal was left intact and the rollback can be retried.
#[napi]
pub fn rollback(workdir: String, target: Option<String>) -> napi::Result<String> {
    let report = open(&workdir)?.rollback(target.as_deref()).map_err(err)?;
    serde_json::to_string(&report).map_err(err)
}

/// Undo the last rollback. Returns a JSON `{ restored, failed }` report.
#[napi]
pub fn redo(workdir: String) -> napi::Result<String> {
    let report = open(&workdir)?.redo().map_err(err)?;
    serde_json::to_string(&report).map_err(err)
}

/// Selective undo: reverse just one file. Returns a description, or null if the
/// path wasn't tracked.
#[napi]
pub fn revert(workdir: String, path: String) -> napi::Result<Option<String>> {
    open(&workdir)?
        .revert(std::path::Path::new(&path))
        .map_err(err)
}
