use {
    crate::sync::{
        atomic::{AtomicPtr, Ordering},
        Arc, Mutex, MutexGuard,
    },
    std::{
        cell::Cell,
        marker::PhantomData,
        ops::{Deref, DerefMut},
        ptr::null_mut,
    },
};

/// A shared value that can be read on the real-time thread without blocking.
pub struct RealtimeReader<T> {
    shared: Arc<Shared<T>>,
    _marker: PhantomData<Cell<()>>,
}

/// A shared value that can be mutated on a non-real-time thread.
pub struct LockingWriter<T> {
    shared: Mutex<Arc<Shared<T>>>,
}

/// A guard that allows reading the shared value on the real-time thread.
pub struct RealtimeReadGuard<'a, T> {
    shared: &'a Shared<T>,
    value: *mut T,
}

/// A guard that allows mutating the shared value on a non-real-time thread.
///
/// The updated value will only be observed by the real-time thread after the guard is dropped.
pub struct LockingWriteGuard<'a, T> {
    shared: MutexGuard<'a, Arc<Shared<T>>>,
    value: Option<T>,
}

/// Creates a shared value that can be read on the real-time thread without blocking.
pub fn realtime_reader<T>(value: T) -> (LockingWriter<T>, RealtimeReader<T>) {
    let value = Box::into_raw(Box::new(value));

    let shared = Arc::new(Shared {
        live: AtomicPtr::new(value),
        storage: AtomicPtr::new(value),
    });

    (
        LockingWriter {
            shared: Mutex::new(shared.clone()),
        },
        RealtimeReader {
            shared,
            _marker: PhantomData,
        },
    )
}

struct Shared<T> {
    storage: AtomicPtr<T>,
    live: AtomicPtr<T>,
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
    pub fn read(&self) -> RealtimeReadGuard<'_, T> {
        let value = self.shared.live.swap(null_mut(), Ordering::SeqCst);
        assert_ne!(value, null_mut());

        RealtimeReadGuard {
            shared: &self.shared,
            value,
        }
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

impl<T> LockingWriter<T>
where
    T: Clone,
{
    /// Mutate the shared value on a non-real-time thread.
    pub fn write(&self) -> LockingWriteGuard<'_, T> {
        let shared = self.shared.lock().unwrap();

        let value_ptr = shared.storage.load(Ordering::Relaxed);

        assert_ne!(value_ptr, null_mut());
        let value = unsafe { &*value_ptr };

        LockingWriteGuard {
            shared,
            value: Some(value.clone()),
        }
    }
}

impl<T> Drop for LockingWriteGuard<'_, T> {
    fn drop(&mut self) {
        let old = self.shared.storage.load(Ordering::Acquire);

        let new = Box::into_raw(Box::new(self.value.take().expect("value taken twice")));
        while self
            .shared
            .live
            .compare_exchange_weak(old, new, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            #[cfg(loom)]
            loom::thread::yield_now();
        }
        assert_ne!(old, null_mut());

        self.shared.storage.store(new, Ordering::Release);

        // SAFETY: No other references to `old` exist, so it's safe to drop.
        let _ = unsafe { Box::from_raw(old) };
    }
}

impl<T> Deref for LockingWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value.as_ref().expect("value taken twice")
    }
}

impl<T> DerefMut for LockingWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().expect("value taken twice")
    }
}

#[cfg(test)]
mod test {
    use {super::*, std::thread};

    #[test]
    fn read_and_write_to_shared_value() {
        let (writer, reader) = realtime_reader(0);

        assert_eq!(*reader.read(), 0);
        *writer.write() += 1;
        assert_eq!(*reader.read(), 1);
        *writer.write() += 1;
        assert_eq!(*reader.read(), 2);
    }

    #[test]
    fn read_and_writing_simultaneously() {
        let (writer, reader) = realtime_reader(0);

        let writer = Arc::new(writer);

        let writers = (0..10)
            .map(|_| {
                thread::spawn({
                    let writer = Arc::clone(&writer);
                    move || {
                        for _ in 0..100 {
                            *writer.write() += 1;
                        }
                    }
                })
            })
            .collect::<Vec<_>>();

        let mut last_value = 0;
        while !writers.iter().all(|writer| writer.is_finished()) {
            let value = *reader.read();
            assert!(value >= last_value);
            assert!(value <= 1_000);
            last_value = value;
        }

        assert_eq!(*reader.read(), 1_000);
    }
}
