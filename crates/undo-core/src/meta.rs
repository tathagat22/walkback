//! Capturing and restoring file metadata. A byte-perfect restore that loses the
//! executable bit on a script, or makes a `600` secrets file world-readable, is
//! a correctness *and* a security failure — so we preserve unix mode and mtime.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

/// The metadata we capture for a regular file or directory.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct Meta {
    /// Unix permission bits (0 on platforms without them).
    #[serde(default)]
    pub mode: u32,
    /// Modification time, whole seconds since the unix epoch.
    #[serde(default)]
    pub mtime_s: i64,
    /// Sub-second nanoseconds of the modification time.
    #[serde(default)]
    pub mtime_ns: u32,
}

/// Capture mode + mtime from an existing path (does not follow symlinks).
pub fn capture(path: &Path) -> io::Result<Meta> {
    let m = fs::symlink_metadata(path)?;
    let (mtime_s, mtime_ns) = mtime_of(&m);
    Ok(Meta {
        mode: mode_of(&m),
        mtime_s,
        mtime_ns,
    })
}

/// Re-apply captured mode + mtime to a path. Best-effort on mtime.
pub fn apply(path: &Path, meta: Meta) -> io::Result<()> {
    set_mode(path, meta.mode)?;
    let ft = filetime::FileTime::from_unix_time(meta.mtime_s, meta.mtime_ns);
    // mtime is a nicety (build systems, reproducibility); never fail the
    // restore over it.
    let _ = filetime::set_file_mtime(path, ft);
    Ok(())
}

/// Re-apply just the unix mode (used for directories, where mtime is volatile).
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    apply_mode(path, mode)
}

#[cfg(unix)]
fn mode_of(m: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.mode()
}

#[cfg(not(unix))]
fn mode_of(_m: &fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn apply_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if mode != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

fn mtime_of(m: &fs::Metadata) -> (i64, u32) {
    match m.modified() {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
            Err(e) => (-(e.duration().as_secs() as i64), 0),
        },
        Err(_) => (0, 0),
    }
}
