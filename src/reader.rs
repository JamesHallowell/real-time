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
    std::{marker::PhantomData, ops::Deref, ptr::null_mut},
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
    value: *mut T,
}

/// Creates a shared value that can be read on the real-time thread without blocking.
pub fn realtime_reader<T>(value: T) -> (LockingWriter<T>, RealtimeReader<T>)
where
    T: Send,
{
    let value = Box::into_raw(Box::new(value));

    let shared = Arc::new(Shared {
        live: CachePadded::new(AtomicPtr::new(value)),
        storage: CachePadded::new(AtomicPtr::new(value)),
        _marker: PhantomData,
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
    storage: CachePadded<AtomicPtr<T>>,
    live: CachePadded<AtomicPtr<T>>,
    _marker: PhantomData<T>,
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        let ptr = self.storage.load(Ordering::Relaxed);
        assert_ne!(ptr, null_mut());

        // SAFETY: No other references to `ptr` exist, so it's safe to drop.
        let _ = unsafe { Box::from_raw(ptr) };
    }
}

impl<T> RealtimeReader<T> {
    /// Read the shared value on the real-time thread.
    pub fn lock(&self) -> RealtimeReadGuard<'_, T> {
        let value = self.shared.live.swap(null_mut(), Ordering::SeqCst);
        assert_ne!(value, null_mut());

        RealtimeReadGuard {
            shared: &self.shared,
            value,
        }
    }

    /// Clone the shared value and return it.
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.lock().clone()
    }
}

impl<T> Drop for RealtimeReadGuard<'_, T> {
    fn drop(&mut self) {
        self.shared.live.store(self.value, Ordering::SeqCst);
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

    /// Update the value.
    pub fn update(&self, mut updater: impl FnMut(T) -> T)
    where
        T: Clone + Send + Sync,
    {
        let value = self.shared.storage.load(Ordering::Relaxed);
        assert_ne!(value, null_mut());
        let value = unsafe { &*value }.clone();

        self.set(updater(value));
    }

    /// Update the value and return the previous value.
    pub fn swap(&self, value: Box<T>) -> Box<T>
    where
        T: Send,
    {
        let old = self.shared.storage.load(Ordering::Acquire);
        assert_ne!(old, null_mut());

        let new = Box::into_raw(value);

        let backoff = Backoff::default();
        while self
            .shared
            .live
            .compare_exchange_weak(old, new, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            backoff.spin();
        }

        self.shared.storage.store(new, Ordering::Release);

        // SAFETY: No other references to `old` exist, so we can reconstruct the box.
        unsafe { Box::from_raw(old) }
    }
}

#[cfg(test)]
mod test {
    use {
        super::*,
        static_assertions::{assert_impl_all, assert_not_impl_all},
        std::{sync::Mutex, thread},
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
    fn updating_the_value() {
        let (writer, reader) = realtime_reader(0);

        writer.update(|value| value + 5);
        assert_eq!(reader.get(), 5);
    }

    #[test]
    fn reading_and_writing_simultaneously_from_different_threads() {
        let (writer, reader) = realtime_reader(0);
        let shared_writer = Arc::new(Mutex::new(writer));

        const NUM_WRITER_THREADS: usize = 10;
        const WRITES_PER_THREAD: usize = 100;

        let writer_threads = (0..NUM_WRITER_THREADS)
            .map(|_| {
                thread::spawn({
                    let shared_writer = Arc::clone(&shared_writer);
                    move || {
                        for _ in 0..WRITES_PER_THREAD {
                            shared_writer.lock().unwrap().update(|value| value + 1);
                        }
                    }
                })
            })
            .collect::<Vec<_>>();

        let mut last_value = 0;
        while !writer_threads
            .iter()
            .all(|writer_thread| writer_thread.is_finished())
        {
            let value = reader.lock();
            assert!(*value >= last_value);
            assert!(*value <= NUM_WRITER_THREADS * WRITES_PER_THREAD);
            last_value = *value;
        }

        assert_eq!(reader.get(), NUM_WRITER_THREADS * WRITES_PER_THREAD);
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
