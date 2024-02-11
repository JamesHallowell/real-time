use {
    crate::{
        sync::{
            atomic::{AtomicPtr, Ordering},
            Arc, Mutex, MutexGuard,
        },
        PhantomUnsync,
    },
    std::{
        marker::PhantomData,
        ops::{Deref, DerefMut},
        ptr::null_mut,
    },
};

/// A shared value that can be read on the real-time thread without blocking.
pub struct RealtimeReader<T> {
    shared: Arc<Shared<T>>,
    _marker: PhantomUnsync,
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

impl<T> Shared<T> {
    fn load(&self) -> T
    where
        T: Clone,
    {
        let value = self.storage.load(Ordering::Relaxed);
        assert_ne!(value, null_mut());

        unsafe { &*value }.clone()
    }

    fn swap(&self, value: Box<T>) -> Box<T> {
        let old = self.storage.load(Ordering::Acquire);
        assert_ne!(old, null_mut());

        let new = Box::into_raw(value);
        while self
            .live
            .compare_exchange_weak(old, new, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            #[cfg(loom)]
            loom::thread::yield_now();
        }

        self.storage.store(new, Ordering::Release);

        // SAFETY: No other references to `old` exist, so we can reconstruct the box.
        unsafe { Box::from_raw(old) }
    }

    fn store(&self, value: T) {
        let _ = self.swap(Box::new(value));
    }
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
    pub fn read(&mut self) -> RealtimeReadGuard<'_, T> {
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

impl<T> LockingWriter<T> {
    /// Set the value.
    pub fn set(&self, value: T) {
        self.shared.lock().unwrap().store(value);
    }

    /// Update the value.
    pub fn update<R>(&self, mut updater: impl FnMut(&mut T) -> R) -> R
    where
        T: Clone,
    {
        let mut writer = self.write();
        updater(&mut *writer)
    }

    /// Mutate the shared value on a non-real-time thread.
    pub fn write(&self) -> LockingWriteGuard<'_, T>
    where
        T: Clone,
    {
        let shared = self.shared.lock().unwrap();

        let value = Some(shared.load());
        LockingWriteGuard { shared, value }
    }

    /// Update the value and return the previous value.
    pub fn swap(&self, value: Box<T>) -> Box<T> {
        self.shared.lock().unwrap().swap(value)
    }
}

impl<T> Drop for LockingWriteGuard<'_, T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            self.shared.store(value);
        }
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
        let (writer, mut reader) = realtime_reader(0);

        assert_eq!(*reader.read(), 0);
        *writer.write() += 1;
        assert_eq!(*reader.read(), 1);
        *writer.write() += 1;
        assert_eq!(*reader.read(), 2);
    }

    #[test]
    fn setting_the_value() {
        let (writer, mut reader) = realtime_reader(0);

        writer.set(5);
        assert_eq!(*reader.read(), 5);
    }

    #[test]
    fn updating_the_value() {
        let (writer, mut reader) = realtime_reader(0);

        writer.update(|value| *value = 5);
        assert_eq!(*reader.read(), 5);
    }

    #[test]
    fn read_and_writing_simultaneously() {
        let (writer, mut reader) = realtime_reader(0);

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

    #[test]
    fn swapping_the_value() {
        use std::ptr::addr_of;

        let a = Box::new(1);
        let addr_of_a = addr_of!(*a);

        let (writer, mut reader) = realtime_reader(0);

        let mut b = writer.swap(a);
        assert_eq!(*reader.read(), 1);
        *b = 2;

        let c = writer.swap(b);
        assert_eq!(*reader.read(), 2);
        assert_eq!(addr_of!(*c), addr_of_a);
    }
}
