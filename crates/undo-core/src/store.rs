//! A tiny content-addressed blob store, git-style. We capture the *prior*
//! contents of any file an agent touches, keyed by their SHA-256, so that even
//! large or binary files restore byte-perfect. Identical contents are stored
//! once.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn ensure(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)
    }

    fn hash_hex(data: &[u8]) -> String {
        let digest = Sha256::digest(data);
        let mut s = String::with_capacity(64);
        for b in digest {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// Store raw bytes, returning their content hash. Writing is race-free: a
    /// concurrent writer of the same content simply loses the `create_new` and
    /// we treat that as success (the bytes are identical by construction).
    pub fn put_bytes(&self, data: &[u8]) -> io::Result<String> {
        self.ensure()?;
        let hash = Self::hash_hex(data);
        let path = self.root.join(&hash);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(data)?;
                f.sync_all()?;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        Ok(hash)
    }

    /// Capture the current contents of a file into the store.
    pub fn put_file(&self, path: &Path) -> io::Result<String> {
        let data = fs::read(path)?;
        self.put_bytes(&data)
    }

    /// Fetch previously stored bytes by hash.
    pub fn get(&self, hash: &str) -> io::Result<Vec<u8>> {
        fs::read(self.root.join(hash))
    }
}
