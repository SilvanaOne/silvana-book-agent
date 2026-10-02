//! Poison-tolerant std lock access and validated semaphore construction.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use anyhow::{Result, bail};
use tokio::sync::Semaphore;

/// Largest permit count `semaphore` accepts.
pub const MAX_PERMITS: usize = 1024;

/// Lock a mutex, recovering the guard if a previous holder panicked.
pub fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Read-lock an `RwLock`, recovering the guard if a writer panicked.
pub fn read<T: ?Sized>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(PoisonError::into_inner)
}

/// Write-lock an `RwLock`, recovering the guard if a writer panicked.
pub fn write<T: ?Sized>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

/// A semaphore with `n` permits; `n` must be in `1..=MAX_PERMITS`.
#[expect(clippy::disallowed_methods, reason = "the one validated construction site")]
pub fn semaphore(n: usize) -> Result<Semaphore> {
    if !(1..=MAX_PERMITS).contains(&n) {
        bail!("semaphore permit count {n} outside 1..={MAX_PERMITS}");
    }
    Ok(Semaphore::new(n.min(Semaphore::MAX_PERMITS)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn poison(f: impl FnOnce() + Send + 'static) {
        assert!(std::thread::spawn(f).join().is_err(), "the helper thread must panic");
    }

    // A panic while holding the lock poisons it; later callers still get the value
    #[test]
    fn lock_survives_poison() {
        let m = Arc::new(Mutex::new(5));
        let m2 = m.clone();
        poison(move || {
            let mut g = m2.lock().unwrap();
            *g = 6;
            panic!("poison the mutex");
        });
        assert!(m.is_poisoned());
        assert_eq!(*lock(&m), 6);
        *lock(&m) = 7;
        assert_eq!(*lock(&m), 7);
    }

    #[test]
    fn read_and_write_survive_poison() {
        let l = Arc::new(RwLock::new(String::from("a")));
        let l2 = l.clone();
        poison(move || {
            let mut g = l2.write().unwrap();
            g.push('b');
            panic!("poison the rwlock");
        });
        assert!(l.is_poisoned());
        assert_eq!(read(&l).as_str(), "ab");
        write(&l).push('c');
        assert_eq!(read(&l).as_str(), "abc");
    }

    // Semaphore::new panics above its own maximum; out-of-range counts are errors here
    #[test]
    fn semaphore_rejects_out_of_range_counts() {
        assert!(semaphore(0).is_err());
        assert!(semaphore(MAX_PERMITS + 1).is_err());
        assert!(semaphore(usize::MAX).is_err());
        assert_eq!(semaphore(1).unwrap().available_permits(), 1);
        assert_eq!(semaphore(MAX_PERMITS).unwrap().available_permits(), MAX_PERMITS);
    }
}
