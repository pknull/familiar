//! Content-addressed snapshot store for file-level revert.
//!
//! Stores file content by SHA-256 hash in `~/.familiar/snapshots/<hash>`.
//! Integrates with the sessions table to record which files changed per session.
//! Orphaned snapshots (no session references) are cleaned during session prune.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{FamiliarError, Result};
use crate::store::Store;

/// Content-addressed file snapshot store.
pub struct SnapshotStore {
    dir: PathBuf,
}

impl SnapshotStore {
    /// Open or create the snapshot store at the given directory.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| FamiliarError::Internal {
                reason: format!("failed to create snapshot directory: {}", e),
            })?;
        }
        Ok(Self { dir })
    }

    /// Store file content and return its hash.
    pub fn store(&self, content: &[u8]) -> Result<String> {
        let hash = hex_sha256(content);
        let path = self.dir.join(&hash);

        if !path.exists() {
            std::fs::write(&path, content).map_err(|e| FamiliarError::Internal {
                reason: format!("failed to write snapshot {}: {}", hash, e),
            })?;
        }

        Ok(hash)
    }

    /// Retrieve file content by hash. Validates hash format to prevent path traversal.
    pub fn retrieve(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        if !is_valid_hash(hash) {
            return Err(FamiliarError::Internal {
                reason: format!("invalid snapshot hash: {}", hash),
            });
        }
        let path = self.dir.join(hash);
        if path.exists() {
            let content = std::fs::read(&path).map_err(|e| FamiliarError::Internal {
                reason: format!("failed to read snapshot {}: {}", hash, e),
            })?;
            Ok(Some(content))
        } else {
            Ok(None)
        }
    }

    /// Remove orphaned snapshots not referenced by any session.
    /// Returns the number of files removed.
    pub fn sweep_orphans(&self, store: &Store) -> Result<usize> {
        let referenced = store.all_snapshot_hashes()?;
        let mut removed = 0;

        let entries = std::fs::read_dir(&self.dir).map_err(|e| FamiliarError::Internal {
            reason: format!("failed to read snapshot directory: {}", e),
        })?;

        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            if !referenced.contains(&name) {
                if let Ok(()) = std::fs::remove_file(entry.path()) {
                    removed += 1;
                }
            }
        }

        Ok(removed)
    }
}

/// Record a file snapshot: store content and record in session.
pub fn snapshot_file(
    snapshot_store: &SnapshotStore,
    db_store: &Store,
    session_id: &str,
    file_path: &str,
    content: &[u8],
) -> Result<String> {
    let hash = snapshot_store.store(content)?;
    db_store.record_snapshot(session_id, file_path, &hash)?;
    Ok(hash)
}

/// Revert a file to a specific snapshot point.
pub fn revert_file(
    snapshot_store: &SnapshotStore,
    db_store: &Store,
    session_id: &str,
    file_path: &str,
    snapshot_id: i64,
) -> Result<()> {
    let snapshots = db_store.get_snapshots(session_id)?;

    let target = snapshots
        .iter()
        .find(|(id, path, _, _)| *id == snapshot_id && path == file_path)
        .ok_or_else(|| FamiliarError::Internal {
            reason: format!("snapshot {} not found for file {}", snapshot_id, file_path),
        })?;

    let content = snapshot_store
        .retrieve(&target.2)?
        .ok_or_else(|| FamiliarError::Internal {
            reason: format!("snapshot content {} not found on disk", target.2),
        })?;

    std::fs::write(file_path, content).map_err(|e| FamiliarError::Internal {
        reason: format!("failed to revert file {}: {}", file_path, e),
    })?;

    Ok(())
}

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Validate that a hash is exactly 64 lowercase hex characters (SHA-256).
fn is_valid_hash(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve() {
        let dir = tempfile::tempdir().unwrap();
        let ss = SnapshotStore::new(dir.path()).unwrap();

        let content = b"hello world";
        let hash = ss.store(content).unwrap();
        assert!(!hash.is_empty());

        let retrieved = ss.retrieve(&hash).unwrap().unwrap();
        assert_eq!(retrieved, content);
    }

    #[test]
    fn dedup_same_content() {
        let dir = tempfile::tempdir().unwrap();
        let ss = SnapshotStore::new(dir.path()).unwrap();

        let h1 = ss.store(b"duplicate").unwrap();
        let h2 = ss.store(b"duplicate").unwrap();
        assert_eq!(h1, h2);

        // Only one file on disk
        let count = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(count, 1);
    }

    #[test]
    fn missing_hash_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let ss = SnapshotStore::new(dir.path()).unwrap();
        // Valid hash format but not stored.
        let fake_hash = "a".repeat(64);
        assert!(ss.retrieve(&fake_hash).unwrap().is_none());
    }

    #[test]
    fn invalid_hash_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ss = SnapshotStore::new(dir.path()).unwrap();
        assert!(ss.retrieve("../../../etc/passwd").is_err());
        assert!(ss.retrieve("nonexistent").is_err());
    }
}
