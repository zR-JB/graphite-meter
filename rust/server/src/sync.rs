//! One policy for poisoned locks. A panic that held a lock is a bug its panic has already reported; failing every
//! later request that needs the lock would turn it into an outage. So a poisoned lock is recovered, and reported
//! once, as its state may be inaccurate.
use std::sync::{LockResult, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub(crate) fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    recover::<T, _>(mutex.lock(), || mutex.clear_poison())
}

pub(crate) fn read<T: ?Sized>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    recover::<T, _>(lock.read(), || lock.clear_poison())
}

pub(crate) fn write<T: ?Sized>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    recover::<T, _>(lock.write(), || lock.clear_poison())
}

fn recover<T: ?Sized, G>(result: LockResult<G>, clear: impl FnOnce()) -> G {
    result.unwrap_or_else(|poisoned| {
        crate::log!(
            "[gm:server] {} recovered after a panic and may be inaccurate",
            std::any::type_name::<T>()
        );
        clear();
        poisoned.into_inner()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poisoned_lock_is_recovered_once_with_its_state() {
        let counts = Mutex::new(1);
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let mut counts = lock(&counts);
                    *counts += 1;
                    panic!("a bug under the lock");
                })
                .join()
                .unwrap_err();
        });
        assert!(counts.is_poisoned());
        assert_eq!(*lock(&counts), 2);
        assert!(!counts.is_poisoned(), "recovery is reported once");
    }
}
