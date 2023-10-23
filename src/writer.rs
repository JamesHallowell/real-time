use {
    crate::{
        sync::{
            atomic::{AtomicU8, Ordering},
            Arc, Mutex, MutexGuard,
        },
        PhantomUnsync,
    },
    std::{
        cell::UnsafeCell,
        marker::PhantomData,
        ops::{Deref, DerefMut},
    },
};

/// A shared value that can read on a non-real-time thread.
pub struct LockingReader<T> {
    shared: Mutex<Arc<Shared<T>>>,
}

/// A shared value that can be mutated on the real-time thread without blocking.
pub struct RealtimeWriter<T> {
    shared: Arc<Shared<T>>,
    value: T,
    _marker: PhantomUnsync,
}

/// A guard that allows reading the shared value on a non-real-time thread.
pub struct ReadGuard<'a, T> {
    lock: MutexGuard<'a, Arc<Shared<T>>>,
}

/// A guard that allows mutating the shared value on the real-time thread.
///
/// The updated value will only be observed by any non-real-time thread after the guard is dropped.
pub struct WriteGuard<'a, T>
where
    T: Copy,
{
    writer: &'a mut RealtimeWriter<T>,
}

/// Creates a shared value that can be mutated on the real-time thread without blocking.
pub fn realtime_writer<T>(value: T) -> (LockingReader<T>, RealtimeWriter<T>)
where
    T: Copy,
{
    let shared = Arc::new(Shared {
        values: [UnsafeCell::new(value), UnsafeCell::new(value)],
        control: AtomicU8::new(0),
    });

    (
        LockingReader {
            shared: Mutex::new(shared.clone()),
        },
        RealtimeWriter {
            shared,
            value,
            _marker: PhantomData,
        },
    )
}

struct Shared<T> {
    values: [UnsafeCell<T>; 2],
    control: AtomicU8,
}

unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}

#[repr(u8)]
enum ControlBit {
    Index = 0b001,
    Busy = 0b010,
    Update = 0b100,
}

trait ControlBits {
    fn index(self) -> usize;
    fn is_bit_set(&self, bit: ControlBit) -> bool;
    fn with_bit_set(self, bit: ControlBit) -> Self;
    fn with_bit_unset(self, bit: ControlBit) -> Self;
    fn with_bit_flipped(self, bit: ControlBit) -> Self;
    fn bitwise_and(self, other: ControlBit) -> Self;
}

impl ControlBits for u8 {
    fn index(self) -> usize {
        (self & ControlBit::Index as u8) as usize
    }

    fn is_bit_set(&self, bit: ControlBit) -> bool {
        self & (bit as u8) != 0
    }

    fn with_bit_set(self, bit: ControlBit) -> Self {
        self | (bit as u8)
    }

    fn with_bit_unset(self, bit: ControlBit) -> Self {
        self & !(bit as u8)
    }

    fn with_bit_flipped(self, bit: ControlBit) -> Self {
        self ^ (bit as u8)
    }

    fn bitwise_and(self, bit: ControlBit) -> Self {
        self & bit as u8
    }
}

impl<T> Shared<T> {
    pub fn write(&self, value: T) {
        let control = self
            .control
            .fetch_or(ControlBit::Busy as u8, Ordering::Acquire);

        {
            let write_slot = &self.values[control.index()];
            let write_slot = unsafe { &mut *write_slot.get() };

            *write_slot = value;
        }

        self.control
            .store(control.with_bit_set(ControlBit::Update), Ordering::Release);
    }

    pub fn read(&self) -> &T {
        let control = self.control.load(Ordering::Acquire);

        let index = control
            .is_bit_set(ControlBit::Update)
            .then(|| {
                let mut current = control;
                loop {
                    match self.control.compare_exchange_weak(
                        current.with_bit_unset(ControlBit::Busy),
                        current
                            .bitwise_and(ControlBit::Index)
                            .with_bit_flipped(ControlBit::Index),
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    ) {
                        Ok(current) => return current,
                        Err(actual_current) => {
                            current = actual_current;
                        }
                    }

                    #[cfg(loom)]
                    loom::thread::yield_now();
                }
            })
            .unwrap_or(control.with_bit_flipped(ControlBit::Index))
            .index();

        unsafe { &*self.values[index].get() }
    }
}

impl<T> LockingReader<T> {
    /// Lock the shared value for reading on a non-real-time thread.
    pub fn lock(&self) -> ReadGuard<'_, T> {
        let lock = self.shared.lock().unwrap();
        ReadGuard { lock }
    }
}

impl<T> RealtimeWriter<T> {
    /// Set the shared value any make the update immediately available to any non-real-time threads.
    pub fn set(&mut self, value: T) {
        self.shared.write(value);
    }

    /// Modify the shared value on the real-time thread.
    pub fn write(&mut self) -> WriteGuard<'_, T>
    where
        T: Copy,
    {
        WriteGuard { writer: self }
    }
}

impl<T> Deref for ReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.lock.read()
    }
}

impl<T> Deref for WriteGuard<'_, T>
where
    T: Copy,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.writer.value
    }
}

impl<T> DerefMut for WriteGuard<'_, T>
where
    T: Copy,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.writer.value
    }
}

impl<T> Drop for WriteGuard<'_, T>
where
    T: Copy,
{
    fn drop(&mut self) {
        self.writer.shared.write(self.writer.value);
    }
}

#[cfg(test)]
mod test {
    use {super::*, std::thread};

    #[test]
    fn working_with_control_bits() {
        assert_eq!(0b000.index(), 0);
        assert_eq!(0b000.with_bit_set(ControlBit::Index).index(), 1);
        assert_eq!(0b010.with_bit_unset(ControlBit::Busy), 0b000);
        assert!(0b100.is_bit_set(ControlBit::Update));
        assert_eq!(0b010.with_bit_flipped(ControlBit::Busy), 0b000);
    }

    #[test]
    fn multiple_reads_before_new_writes_dont_read_old_data() {
        let (reader, mut writer) = realtime_writer(0);

        assert_eq!(*reader.lock(), 0);

        writer.set(1);

        assert_eq!(*reader.lock(), 1);
        assert_eq!(*reader.lock(), 1);

        writer.set(2);

        assert_eq!(*reader.lock(), 2);
        assert_eq!(*reader.lock(), 2);
    }

    #[test]
    fn read_and_writing_simultaneously() {
        let (reader, mut writer) = realtime_writer(0);

        let reader = Arc::new(reader);

        let readers = (0..10)
            .map(|_| {
                thread::spawn({
                    let reader = Arc::clone(&reader);
                    move || {
                        while *reader.lock() < 100 {
                            thread::yield_now();
                        }
                    }
                })
            })
            .collect::<Vec<_>>();

        for i in 0..=100 {
            writer.set(i);
        }

        for reader in readers {
            reader.join().unwrap();
        }
    }
}
