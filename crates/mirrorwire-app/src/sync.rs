//! Poison-tolerant mutex locking.
//!
//! A panic in another thread poisons a `Mutex`; the guarded status/settings data stays valid
//! for our uses (plain values, no multi-step invariants), so we keep going instead of
//! turning one thread's panic into a cascade (AGENTS.md 1.9).

use std::sync::{Mutex, MutexGuard, PoisonError};

pub trait LockExt<T> {
    fn lock_ok(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_ok(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn lock_ok_survives_poison() {
        let m = Arc::new(Mutex::new(5));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(m.is_poisoned());
        assert_eq!(*m.lock_ok(), 5);
    }
}
