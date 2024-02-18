use {
    crate::{
        sync::{
            atomic::{AtomicU8, Ordering},
            Arc,
        },
        PhantomUnsync,
    },
    std::{cell::UnsafeCell, hint, marker::PhantomData},
};

/// A shared value that can read on a non-real-time thread.
pub struct LockingReader<T> {
    shared: Arc<Shared<T>>,
}

/// A shared value that can be mutated on the real-time thread without blocking.
pub struct RealtimeWriter<T> {
    shared: Arc<Shared<T>>,
    _marker: PhantomUnsync,
}

/// Creates a shared value that can be mutated on the real-time thread without blocking.
pub fn realtime_writer<T>(value: T) -> (LockingReader<T>, RealtimeWriter<T>)
where
    T: Copy + Send,
{
    let shared = Arc::new(Shared {
        values: [UnsafeCell::new(value), UnsafeCell::new(value)],
        control: AtomicU8::new(0),
    });

    (
        LockingReader {
            shared: Arc::clone(&shared),
        },
        RealtimeWriter {
            shared,
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
    NewData = 0b100,
}

impl From<ControlBit> for u8 {
    fn from(bit: ControlBit) -> u8 {
        bit as u8
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
struct ControlBits(u8);

impl ControlBits {
    fn is_set(&self, bit: ControlBit) -> bool {
        self.0 & (bit as u8) != 0
    }
    fn set(self, bit: ControlBit) -> Self {
        (self.0 | (bit as u8)).into()
    }
    fn unset(self, bit: ControlBit) -> Self {
        (self.0 & !(bit as u8)).into()
    }
    fn flip(self, bit: ControlBit) -> Self {
        (self.0 ^ (bit as u8)).into()
    }
    fn bitwise_and(self, bit: ControlBit) -> Self {
        (self.0 & (bit as u8)).into()
    }
    fn write_index(self) -> usize {
        self.bitwise_and(ControlBit::Index).0 as usize
    }
    fn read_index(self) -> usize {
        self.bitwise_and(ControlBit::Index)
            .flip(ControlBit::Index)
            .0 as usize
    }
}

impl From<ControlBits> for u8 {
    fn from(bits: ControlBits) -> u8 {
        bits.0
    }
}

impl From<u8> for ControlBits {
    fn from(byte: u8) -> ControlBits {
        ControlBits(byte)
    }
}

impl<T> LockingReader<T> {
    /// Read the shared value on the non-real-time thread.
    pub fn get(&mut self) -> T
    where
        T: Send + Copy,
    {
        *self.get_ref()
    }

    /// Read the shared value on the non-real-time thread.
    pub fn get_ref(&mut self) -> &T
    where
        T: Send,
    {
        let control: ControlBits = self.shared.control.load(Ordering::SeqCst).into();

        let read_index = control
            .is_set(ControlBit::NewData)
            .then(|| {
                let mut control = control;
                loop {
                    // Wait until the writer has finished writing...
                    let current = control.unset(ControlBit::Busy);

                    // ...and then swap the read and write slots, also clearing the new data bit.
                    let new = current.unset(ControlBit::NewData).flip(ControlBit::Index);

                    match self
                        .shared
                        .control
                        .compare_exchange_weak(
                            current.into(),
                            new.into(),
                            Ordering::AcqRel,
                            Ordering::Relaxed,
                        )
                        .map(ControlBits)
                    {
                        Ok(previous) => {
                            return previous.write_index();
                        }
                        Err(actual) => {
                            control = actual.into();
                            hint::spin_loop();

                            #[cfg(loom)]
                            loom::thread::yield_now();
                        }
                    }
                }
            })
            .unwrap_or(control.read_index());

        // SAFETY: We have unique access to the value in the read slot.
        unsafe { &*self.shared.values[read_index].get() }
    }
}

impl<T> RealtimeWriter<T> {
    /// Set the shared value and make the update immediately available to any non-real-time threads.
    pub fn set(&mut self, value: T)
    where
        T: Send,
    {
        // Set the busy bit to prevent the reader from swapping the slots whilst we are writing.
        let control: ControlBits = self
            .shared
            .control
            .fetch_or(ControlBit::Busy.into(), Ordering::Acquire)
            .into();

        {
            let write_slot = &self.shared.values[control.write_index()];

            // SAFETY: We have unique access to the value in the write slot.
            let write_value = unsafe { &mut *write_slot.get() };
            *write_value = value;
        }

        // Tell the reader that new data is available and clear the busy bit.
        self.shared
            .control
            .store(control.set(ControlBit::NewData).into(), Ordering::Release);
    }
}

#[cfg(test)]
mod test {
    use {
        super::*,
        std::{sync::Mutex, thread},
    };

    #[test]
    fn determining_the_read_and_write_indexes() {
        let control = ControlBits(0b000);

        assert_eq!(control.write_index(), 0);
        assert_eq!(control.read_index(), 1);

        let control = control.flip(ControlBit::Index);

        assert_eq!(control.write_index(), 1);
        assert_eq!(control.read_index(), 0);
    }

    #[test]
    fn setting_bits_in_the_control() {
        let control = ControlBits(0b000);

        assert!(!control.is_set(ControlBit::Index));
        assert!(!control.is_set(ControlBit::Busy));
        assert!(!control.is_set(ControlBit::NewData));

        let control = control.set(ControlBit::Busy);

        assert!(!control.is_set(ControlBit::Index));
        assert!(control.is_set(ControlBit::Busy));
        assert!(!control.is_set(ControlBit::NewData));

        let control = control.unset(ControlBit::Busy).set(ControlBit::NewData);

        assert!(!control.is_set(ControlBit::Index));
        assert!(!control.is_set(ControlBit::Busy));
        assert!(control.is_set(ControlBit::NewData));
    }

    #[test]
    fn managing_the_control_bits() {
        let (mut reader, mut writer) = realtime_writer(0);
        let get_controls_bits =
            |writer: &RealtimeWriter<_>| ControlBits(writer.shared.control.load(Ordering::SeqCst));

        {
            let initial_control_bits = get_controls_bits(&writer);
            assert!(!initial_control_bits.is_set(ControlBit::Busy));
            assert!(!initial_control_bits.is_set(ControlBit::NewData));
            assert_eq!(initial_control_bits.write_index(), 0);
            assert_eq!(initial_control_bits.read_index(), 1);
        }

        writer.set(1);

        {
            let control_bits_after_set = get_controls_bits(&writer);
            assert!(!control_bits_after_set.is_set(ControlBit::Busy));
            assert!(control_bits_after_set.is_set(ControlBit::NewData));
            assert_eq!(control_bits_after_set.write_index(), 0);
            assert_eq!(control_bits_after_set.read_index(), 1);
        }

        let _value = reader.get();

        {
            let control_bits_after_get = get_controls_bits(&writer);
            assert!(!control_bits_after_get.is_set(ControlBit::Busy));
            assert!(!control_bits_after_get.is_set(ControlBit::NewData));
            assert_eq!(control_bits_after_get.write_index(), 1);
            assert_eq!(control_bits_after_get.read_index(), 0);
        }

        writer.set(2);

        {
            let control_bits_after_set = get_controls_bits(&writer);
            assert!(!control_bits_after_set.is_set(ControlBit::Busy));
            assert!(control_bits_after_set.is_set(ControlBit::NewData));
            assert_eq!(control_bits_after_set.write_index(), 1);
            assert_eq!(control_bits_after_set.read_index(), 0);
        }
    }

    #[test]
    fn multiple_reads_before_new_writes_dont_read_old_data() {
        let (mut reader, mut writer) = realtime_writer(0);

        assert_eq!(reader.get(), 0);

        writer.set(1);

        assert_eq!(reader.get(), 1);
        assert_eq!(reader.get(), 1);

        writer.set(2);

        assert_eq!(reader.get(), 2);
        assert_eq!(reader.get(), 2);
    }

    #[test]
    fn reading_and_writing_simultaneously() {
        let (reader, mut writer) = realtime_writer(0);

        let shared_reader = Arc::new(Mutex::new(reader));

        let readers = (0..10)
            .map(|_| {
                thread::spawn({
                    let reader = Arc::clone(&shared_reader);
                    move || {
                        let read_value = || {
                            let mut reader = reader.lock().unwrap();
                            reader.get()
                        };

                        let last_value = read_value();
                        loop {
                            let value = read_value();

                            assert!(value <= 1000);
                            assert!(value >= 0);
                            assert!(value >= last_value);

                            if value == 1000 {
                                break;
                            }
                        }
                    }
                })
            })
            .collect::<Vec<_>>();

        for i in 0..=1000 {
            writer.set(i);
        }

        for reader in readers {
            reader.join().unwrap();
        }
    }
}
