use {
    crate::{sync::Arc, PhantomUnsync},
    control::ControlBlock,
    crossbeam_utils::CachePadded,
    std::{cell::UnsafeCell, marker::PhantomData, mem::MaybeUninit},
};

mod control;

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
            CachePadded::new(UnsafeCell::new(MaybeUninit::uninit())),
        ],
        control: ControlBlock::default(),
    });

    let writer = RealtimeWriter {
        shared: Arc::clone(&shared),
        _marker: PhantomData,
    };

    writer.set(value);

    (
        writer,
        LockingReader {
            shared,
            _marker: PhantomData,
        },
    )
}

struct Shared<T> {
    values: [CachePadded<UnsafeCell<MaybeUninit<T>>>; 2],
    control: ControlBlock,
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        for slot in 0..2 {
            if !self.control.is_slot_initialised(slot) {
                continue;
            }

            let slot = self.values[slot].get();

            // SAFETY: We have unique access to both slots, and the slot contains a
            // previously initialised value.
            unsafe {
                slot.as_mut_unchecked().assume_init_drop();
            };
        }
    }
}

unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}

impl<T> LockingReader<T> {
    fn update(&self) -> &T {
        let read_slot_index = self.shared.control.read();
        let read_slot_ptr = self.shared.values[read_slot_index].get();

        // SAFETY: There are no mutable aliases to the read slot.
        let read_slot = unsafe { read_slot_ptr.as_ref_unchecked() };

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
        let value = value.into();

        self.shared.control.write(|index, initialised| {
            let write_slot_ptr = self.shared.values[index].get();

            // SAFETY: We have unique access to the value in the write slot, as we have
            // signalled to the reader that we are currently busy, preventing it from
            // swapping the slots.
            let write_slot = unsafe { write_slot_ptr.as_mut_unchecked() };

            if initialised {
                // SAFETY: The value is in an initialised state as each slot has been written to
                // as indicated by the control bit.
                unsafe {
                    write_slot.assume_init_drop();
                }
            }

            write_slot.write(value);
        });
    }
}

#[cfg(test)]
mod test {
    use {
        super::*,
        static_assertions::{assert_impl_all, assert_not_impl_any},
        std::{
            sync::atomic::{AtomicUsize, Ordering},
            thread,
        },
    };

    assert_impl_all!(RealtimeWriter<i32>: Send);
    assert_not_impl_any!(RealtimeWriter<i32>: Sync, Copy, Clone);

    assert_impl_all!(LockingReader<i32>: Send);
    assert_not_impl_any!(LockingReader<i32>: Sync, Copy, Clone);

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
                DroppableValue(Arc::clone(drop_count))
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
