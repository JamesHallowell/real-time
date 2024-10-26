use {
    crate::{
        backoff::Backoff,
        sync::{
            atomic::{AtomicPtr, Ordering},
            Arc,
        },
        PhantomUnsync,
    },
    crossbeam_utils::CachePadded,
    std::{cell::UnsafeCell, marker::PhantomData, ops::Deref, ptr::null_mut},
};

/// A shared value that can be read on the real-time thread without blocking.
pub struct RealtimeReader<T> {
    shared: Arc<Shared<T>>,
    _marker: PhantomUnsync,
}

/// A shared value that can be mutated on a non-real-time thread.
pub struct LockingWriter<T> {
    shared: Arc<Shared<T>>,
    _marker: PhantomUnsync,
}

/// A guard that allows reading the shared value on the real-time thread.
pub struct RealtimeReadGuard<'a, T> {
    shared: &'a Shared<T>,
    value: *const T,
}

/// Creates a shared value that can be read on the real-time thread without blocking.
pub fn realtime_reader<T>(value: T) -> (LockingWriter<T>, RealtimeReader<T>)
where
    T: Send,
{
    let storage = Box::into_raw(Box::new(value));

    let shared = Arc::new(Shared {
        live: CachePadded::new(AtomicPtr::new(storage)),
        storage: CachePadded::new(UnsafeCell::new(storage)),
    });

    (
        LockingWriter {
            shared: Arc::clone(&shared),
            _marker: PhantomData,
        },
        RealtimeReader {
            shared,
            _marker: PhantomData,
        },
    )
}

struct Shared<T> {
    storage: CachePadded<UnsafeCell<*mut T>>,
    live: CachePadded<AtomicPtr<T>>,
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // SAFETY: Returned pointer of `get` call is never null.
        let value = unsafe { *self.storage.get() };

        assert!(!value.is_null());

        // SAFETY: No other references to `value` exist, so it's safe to drop.
        let _ = unsafe { Box::from_raw(value) };
    }
}

unsafe impl<T> Sync for Shared<T> {}
unsafe impl<T> Send for Shared<T> {}

impl<T> RealtimeReader<T> {
    /// Read the shared value on the real-time thread.
    pub fn lock(&self) -> RealtimeReadGuard<'_, T> {
        let value = self.shared.live.swap(null_mut(), Ordering::SeqCst);
        debug_assert!(!value.is_null());

        RealtimeReadGuard {
            shared: &self.shared,
            value,
        }
    }

    /// Copy the shared value and return it.
    pub fn get(&self) -> T
    where
        T: Copy,
    {
        *self.lock()
    }
}

impl<T> Drop for RealtimeReadGuard<'_, T> {
    fn drop(&mut self) {
        self.shared
            .live
            .store(self.value.cast_mut(), Ordering::SeqCst);
    }
}

impl<T> Deref for RealtimeReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `self.value` is a valid pointer for the lifetime of `self`.
        unsafe { &*self.value }
    }
}

impl<T> LockingWriter<T> {
    /// Set the value.
    pub fn set(&self, value: T)
    where
        T: Send,
    {
        let _ = self.swap(Box::new(value));
    }

    /// Update the value and return the previous value.
    pub fn swap(&self, value: Box<T>) -> Box<T>
    where
        T: Send,
    {
        let new = Box::into_raw(value);

        // SAFETY: Both pointers are valid and aligned as they come from calling `Box::into_raw`.
        let old = unsafe { self.shared.storage.get().replace(new) };

        let backoff = Backoff::default();
        while self
            .shared
            .live
            .compare_exchange_weak(old, new, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            backoff.spin();
        }

        // SAFETY: No other references to `old` now exist, so we can reconstruct the box.
        unsafe { Box::from_raw(old) }
    }
}

#[cfg(test)]
mod test {
    use {
        super::*,
        static_assertions::{assert_impl_all, assert_not_impl_all},
        std::thread,
    };

    assert_impl_all!(RealtimeReader<i32>: Send);
    assert_not_impl_all!(RealtimeReader<i32>: Sync, Copy, Clone);

    assert_impl_all!(LockingWriter<i32>: Send);
    assert_not_impl_all!(LockingWriter<i32>: Sync, Copy, Clone);

    #[test]
    fn setting_and_getting_the_shared_value() {
        let (writer, reader) = realtime_reader(0);

        assert_eq!(reader.get(), 0);
        writer.set(1);
        assert_eq!(reader.get(), 1);
        writer.set(2);
        assert_eq!(reader.get(), 2);
    }

    #[test]
    fn reading_and_writing_simultaneously_from_different_threads() {
        let (writer, reader) = realtime_reader(0);

        #[cfg(miri)]
        const NUM_WRITES: usize = 10;

        #[cfg(not(miri))]
        const NUM_WRITES: usize = 1_000_000;

        let writer_thread = thread::spawn({
            move || {
                for value in 1..=NUM_WRITES {
                    writer.set(value);
                }
            }
        });

        let mut last_value = 0;
        while !writer_thread.is_finished() {
            let value = reader.lock();
            assert!(*value >= last_value);
            assert!(*value <= NUM_WRITES);
            last_value = *value;
        }

        assert_eq!(reader.get(), NUM_WRITES);
    }

    #[test]
    fn swapping_the_value() {
        use std::ptr::addr_of;

        let a = Box::new(1);
        let a_addr = addr_of!(*a);

        let (writer, reader) = realtime_reader(0);

        let mut b = writer.swap(a);
        assert_eq!(reader.get(), 1);
        *b = 2;

        let c = writer.swap(b);
        assert_eq!(reader.get(), 2);
        assert_eq!(addr_of!(*c), a_addr);
    }
}
