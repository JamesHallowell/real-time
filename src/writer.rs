use {
    crate::{
        backoff::Backoff,
        sync::{
            atomic::{AtomicU8, Ordering},
            Arc,
        },
        PhantomUnsync,
    },
    crossbeam_utils::CachePadded,
    std::{cell::UnsafeCell, marker::PhantomData, mem::MaybeUninit},
};

/// A shared value that can read on a non-real-time thread.
pub struct LockingReader<T> {
    shared: Arc<Shared<T>>,
    _marker: PhantomUnsync,
}

/// A shared value that can be mutated on the real-time thread without blocking.
pub struct RealtimeWriter<T> {
    shared: Arc<Shared<T>>,
    _marker: PhantomUnsync,
}

/// Creates a shared value that can be mutated on the real-time thread without
/// blocking.
pub fn writable<T>(value: T) -> (RealtimeWriter<T>, LockingReader<T>)
where
    T: Send,
{
    let shared = Arc::new(Shared {
        values: [
            CachePadded::new(UnsafeCell::new(MaybeUninit::uninit())),
            CachePadded::new(UnsafeCell::new(MaybeUninit::new(value))),
        ],
        control: AtomicU8::new(ControlBits::default().into()),
    });

    (
        RealtimeWriter {
            shared: Arc::clone(&shared),
            _marker: PhantomData,
        },
        LockingReader {
            shared,
            _marker: PhantomData,
        },
    )
}

struct Shared<T> {
    values: [CachePadded<UnsafeCell<MaybeUninit<T>>>; 2],
    control: AtomicU8,
}

impl<T> Shared<T> {
    /// The caller must ensure that no mutations or mutable aliases can occur
    /// for the value at the slot being accessed.
    #[inline]
    unsafe fn get(&self, index: usize) -> &MaybeUninit<T> {
        debug_assert!(index < self.values.len());
        &*self.values[index].get()
    }

    /// The caller must have exclusive access to the value in the slot being
    /// accessed.
    #[inline]
    unsafe fn get_mut(&self, index: usize) -> &mut MaybeUninit<T> {
        debug_assert!(index < self.values.len());
        &mut *self.values[index].get()
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        let control: ControlBits = self.control.load(Ordering::SeqCst).into();

        for slot in 0..self.values.len() {
            if control.is_slot_initialised(slot) {
                // SAFETY: We have unique access to both slots, and the slot contains a
                // previously initialised value.
                unsafe {
                    self.get_mut(slot).assume_init_drop();
                };
            }
        }
    }
}

unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}

#[repr(u8)]
enum ControlBit {
    WriteSlotIndex = 0b001,
    IsWriterBusy = 0b010,
    HasNewData = 0b100,
    IsSlot0Initialised = 0b1000,
}

impl From<ControlBit> for u8 {
    fn from(bit: ControlBit) -> u8 {
        bit as u8
    }
}

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
struct ControlBits(u8);

impl ControlBits {
    fn is_set(&self, bit: ControlBit) -> bool {
        self.0 & (bit as u8) != 0
    }
    fn set(self, bit: ControlBit) -> Self {
        (self.0 | (bit as u8)).into()
    }
    fn clear(self, bit: ControlBit) -> Self {
        (self.0 & !(bit as u8)).into()
    }
    fn flip(self, bit: ControlBit) -> Self {
        (self.0 ^ (bit as u8)).into()
    }
    fn bitwise_and(self, bit: ControlBit) -> Self {
        (self.0 & (bit as u8)).into()
    }
    fn write_slot_index(self) -> usize {
        self.bitwise_and(ControlBit::WriteSlotIndex).0 as usize
    }
    fn read_slot_index(self) -> usize {
        self.bitwise_and(ControlBit::WriteSlotIndex)
            .flip(ControlBit::WriteSlotIndex)
            .0 as usize
    }
    fn is_slot_initialised(self, slot: usize) -> bool {
        debug_assert!(slot < 2);
        match slot {
            0 => self.is_set(ControlBit::IsSlot0Initialised),
            1 => true,
            _ => false,
        }
    }
    fn set_slot_initialised(self, slot: usize) -> Self {
        debug_assert!(slot < 2);
        match slot {
            0 => self.set(ControlBit::IsSlot0Initialised),
            _ => self,
        }
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
    fn update(&self) -> &T {
        let mut control: ControlBits = self.shared.control.load(Ordering::SeqCst).into();

        let read_slot_index = control
            .is_set(ControlBit::HasNewData)
            .then(|| {
                let backoff = Backoff::default();
                loop {
                    // Wait until the writer has finished writing...
                    let current = control.clear(ControlBit::IsWriterBusy);

                    // ...and then swap the read and write slots, also clearing the new data
                    // bit.
                    let new = current
                        .clear(ControlBit::HasNewData)
                        .flip(ControlBit::WriteSlotIndex);

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
                            return previous.write_slot_index();
                        }
                        Err(actual) => {
                            control = actual.into();
                            backoff.spin();
                        }
                    }
                }
            })
            .unwrap_or(control.read_slot_index());

        // SAFETY: There are no mutable aliases to the read slot.
        let read_slot = unsafe { self.shared.get(read_slot_index) };

        // SAFETY: Value is in an initialised state as it is either the value that was
        // initialised in the constructor, or an initialised value written by the writer
        // before it notified us of new data.
        unsafe { read_slot.assume_init_ref() }
    }

    /// Get the latest value.
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.update().clone()
    }

    /// Get a reference to the latest value.
    ///
    /// This method requires a unique borrow of the reader to ensure that the
    /// reference isn't invalidated.
    pub fn get_ref(&mut self) -> &T {
        self.update()
    }
}

impl<T> RealtimeWriter<T> {
    /// Set the shared value and make the update immediately available to any
    /// non-real-time threads.
    pub fn set<V>(&self, value: V)
    where
        V: Into<T>,
        T: Send,
    {
        // Set the busy bit to prevent the reader from swapping the slots whilst we are
        // writing.
        let control: ControlBits = self
            .shared
            .control
            .fetch_or(ControlBit::IsWriterBusy.into(), Ordering::Acquire)
            .into();

        let write_slot_index = control.write_slot_index();

        {
            // SAFETY: We have unique access to the value in the write slot, as we have
            // signalled to the reader that we are currently busy, preventing it from
            // swapping the slots.
            let write_slot = unsafe { self.shared.get_mut(write_slot_index) };

            if control.is_slot_initialised(write_slot_index) {
                // SAFETY: The value is in an initialised state as each slot has been written to
                // as indicated by the control bit.
                unsafe {
                    write_slot.assume_init_drop();
                }
            }

            write_slot.write(value.into());
        }

        let control = control
            .set_slot_initialised(write_slot_index)
            .set(ControlBit::HasNewData);

        // Tell the reader that new data is available and clear the busy bit.
        self.shared.control.store(control.into(), Ordering::Release);
    }
}

#[cfg(test)]
mod test {
    use {
        super::*,
        static_assertions::{assert_impl_all, assert_not_impl_any},
        std::{sync::atomic::AtomicUsize, thread},
    };

    assert_impl_all!(RealtimeWriter<i32>: Send);
    assert_not_impl_any!(RealtimeWriter<i32>: Sync, Copy, Clone);

    assert_impl_all!(LockingReader<i32>: Send);
    assert_not_impl_any!(LockingReader<i32>: Sync, Copy, Clone);

    #[test]
    fn determining_the_read_and_write_indexes() {
        let control = ControlBits(0b000);

        assert_eq!(control.write_slot_index(), 0);
        assert_eq!(control.read_slot_index(), 1);

        let control = control.flip(ControlBit::WriteSlotIndex);

        assert_eq!(control.write_slot_index(), 1);
        assert_eq!(control.read_slot_index(), 0);
    }

    #[test]
    fn setting_bits_in_the_control() {
        let control = ControlBits(0b000);

        assert!(!control.is_set(ControlBit::WriteSlotIndex));
        assert!(!control.is_set(ControlBit::IsWriterBusy));
        assert!(!control.is_set(ControlBit::HasNewData));

        let control = control.set(ControlBit::IsWriterBusy);

        assert!(!control.is_set(ControlBit::WriteSlotIndex));
        assert!(control.is_set(ControlBit::IsWriterBusy));
        assert!(!control.is_set(ControlBit::HasNewData));

        let control = control
            .clear(ControlBit::IsWriterBusy)
            .set(ControlBit::HasNewData);

        assert!(!control.is_set(ControlBit::WriteSlotIndex));
        assert!(!control.is_set(ControlBit::IsWriterBusy));
        assert!(control.is_set(ControlBit::HasNewData));
    }

    #[test]
    fn managing_the_control_bits() {
        let (writer, reader) = writable(0);
        let get_controls_bits =
            |writer: &RealtimeWriter<_>| ControlBits(writer.shared.control.load(Ordering::SeqCst));

        {
            let initial_control_bits = get_controls_bits(&writer);
            assert!(!initial_control_bits.is_set(ControlBit::IsWriterBusy));
            assert!(!initial_control_bits.is_set(ControlBit::HasNewData));
            assert!(!initial_control_bits.is_set(ControlBit::IsSlot0Initialised));
            assert_eq!(initial_control_bits.write_slot_index(), 0);
            assert_eq!(initial_control_bits.read_slot_index(), 1);
        }

        writer.set(1);

        {
            let control_bits_after_set = get_controls_bits(&writer);
            assert!(!control_bits_after_set.is_set(ControlBit::IsWriterBusy));
            assert!(control_bits_after_set.is_set(ControlBit::HasNewData));
            assert!(control_bits_after_set.is_set(ControlBit::IsSlot0Initialised));
            assert_eq!(control_bits_after_set.write_slot_index(), 0);
            assert_eq!(control_bits_after_set.read_slot_index(), 1);
        }

        let _value = reader.get();

        {
            let control_bits_after_get = get_controls_bits(&writer);
            assert!(!control_bits_after_get.is_set(ControlBit::IsWriterBusy));
            assert!(!control_bits_after_get.is_set(ControlBit::HasNewData));
            assert!(control_bits_after_get.is_set(ControlBit::IsSlot0Initialised));
            assert_eq!(control_bits_after_get.write_slot_index(), 1);
            assert_eq!(control_bits_after_get.read_slot_index(), 0);
        }

        writer.set(2);

        {
            let control_bits_after_set = get_controls_bits(&writer);
            assert!(!control_bits_after_set.is_set(ControlBit::IsWriterBusy));
            assert!(control_bits_after_set.is_set(ControlBit::HasNewData));
            assert!(control_bits_after_set.is_set(ControlBit::IsSlot0Initialised));
            assert_eq!(control_bits_after_set.write_slot_index(), 1);
            assert_eq!(control_bits_after_set.read_slot_index(), 0);
        }
    }

    #[test]
    fn multiple_reads_before_new_writes_dont_read_old_data() {
        let (writer, reader) = writable(0);

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
        let (writer, mut reader) = writable(0);

        let reader = thread::spawn(move || {
            let last_value = reader.get();
            loop {
                let value = reader.get_ref();

                assert!(*value <= 1000);
                assert!(*value >= 0);
                assert!(*value >= last_value);

                if *value == 1000 {
                    break;
                }
            }
        });

        for i in 0..=1000 {
            writer.set(i);
        }

        reader.join().unwrap();
    }

    #[test]
    fn all_values_get_dropped() {
        struct DroppableValue(Arc<AtomicUsize>);
        impl DroppableValue {
            fn new(drop_count: &Arc<AtomicUsize>) -> Self {
                DroppableValue(Arc::clone(&drop_count))
            }
        }
        impl Drop for DroppableValue {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        for number_of_sets in 0..3 {
            {
                let drop_count = Arc::new(AtomicUsize::new(0));

                {
                    let (writer, _) = writable(DroppableValue::new(&drop_count));

                    for _ in 0..number_of_sets {
                        writer.set(DroppableValue::new(&drop_count));
                    }
                }

                let expected_drop_count = number_of_sets + 1;
                assert_eq!(drop_count.load(Ordering::SeqCst), expected_drop_count);
            }
        }
    }
}
