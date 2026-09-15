//! Cross-process `create_new` spin lock.
//!
//! Budget: 40 × 15ms (same as the digest-delivery ledger). Callers that
//! time out continue without the lock — best-effort serialize, never a
//! hard fail on a stuck lock file.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

/// Held lock file; removed on drop.
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    /// Try to create `path` exclusively, spinning up to the budget.
    pub fn acquire(path: impl AsRef<Path>) -> Option<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        for _ in 0..40 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Some(Self { path }),
                Err(_) => thread::sleep(Duration::from_millis(15)),
            }
        }
        None
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Read `path`, retrying 4× with 25ms backoff on IO error (mid-write / sharing).
pub fn read_with_retry(path: &Path) -> std::io::Result<Vec<u8>> {
    const TRIES: usize = 4;
    let backoff = Duration::from_millis(25);
    let mut last = None;
    for i in 0..TRIES {
        match fs::read(path) {
            Ok(bytes) => return Ok(bytes),
            Err(err) => {
                last = Some(err);
                if i + 1 < TRIES {
                    thread::sleep(backoff);
                }
            }
        }
    }
    Err(last.expect("at least one attempt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_is_exclusive_until_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let first = FileLock::acquire(&path).expect("first");
        assert!(FileLock::acquire(&path).is_none());
        drop(first);
        assert!(FileLock::acquire(&path).is_some());
    }

    #[test]
    fn read_retry_succeeds_once_the_file_appears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        let writer = {
            let path = path.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(30));
                fs::write(path, b"hello").unwrap();
            })
        };
        let bytes = read_with_retry(&path).expect("retry");
        assert_eq!(bytes, b"hello");
        writer.join().unwrap();
    }
}
