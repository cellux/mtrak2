//! Shared helpers for tests that touch the process current directory.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serializes tests that change the process current directory.
pub fn cwd_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Restores the process current directory when dropped.
pub struct CwdGuard {
    original: PathBuf,
}

/// Captures the current directory for restoration at the end of a test.
pub fn cwd_guard() -> CwdGuard {
    CwdGuard {
        original: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}
