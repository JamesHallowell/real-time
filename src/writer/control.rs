use crate::{
    backoff::Backoff,
    sync::atomic::{AtomicU8, Ordering},
};

#[derive(Debug, Default)]
pub struct ControlBlock {
    control_bits: AtomicU8,
}

impl ControlBlock {
    pub fn write(&self, writer: impl FnOnce(usize, bool)) {
        // Set the busy bit to prevent the reader from swapping the slots whilst we are
        // writing.
        let control: ControlBits = self
            .control_bits
            .fetch_or(ControlBit::IsWriterBusy.into(), Ordering::Acquire)
            .into();

        let index = control.write_slot_index();

        writer(index, control.is_slot_initialised(index));

        let control = control
            .set(match index {
                0 => ControlBit::IsSlot0Initialised,
                1 => ControlBit::IsSlot1Initialised,
                _ => unreachable!(),
            })
            .set(ControlBit::HasNewData);

        // Tell the reader that new data is available and clear the busy bit.
        self.control_bits.store(control.into(), Ordering::Release);
    }

    pub fn read(&self) -> usize {
        let mut control_bits: ControlBits = self.control_bits.load(Ordering::SeqCst).into();

        if control_bits.is_set(ControlBit::HasNewData) {
            let backoff = Backoff::default();
            loop {
                // Wait until the writer has finished writing...
                let current = control_bits.clear(ControlBit::IsWriterBusy);

                // ...and then swap the read and write slots, also clearing the new data
                // bit.
                let new = current
                    .clear(ControlBit::HasNewData)
                    .flip(ControlBit::WriteSlotIndex);

                match self
                    .control_bits
                    .compare_exchange_weak(
                        current.into(),
                        new.into(),
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    )
                    .map(ControlBits::from)
                {
                    Ok(previous) => {
                        return previous.write_slot_index();
                    }
                    Err(actual) => {
                        control_bits = actual.into();
                        backoff.spin();
                    }
                }
            }
        } else {
            control_bits.read_slot_index()
        }
    }

    pub fn is_slot_initialised(&self, slot: usize) -> bool {
        let control_bits: ControlBits = self.control_bits.load(Ordering::SeqCst).into();
        control_bits.is_slot_initialised(slot)
    }
}

#[repr(u8)]
enum ControlBit {
    WriteSlotIndex = 0b00001,
    IsWriterBusy = 0b00010,
    HasNewData = 0b00100,
    IsSlot0Initialised = 0b01000,
    IsSlot1Initialised = 0b10000,
}

impl From<ControlBit> for u8 {
    fn from(bit: ControlBit) -> u8 {
        bit as u8
    }
}

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
struct ControlBits(u8);

impl ControlBits {
    #[inline]
    fn is_set(&self, bit: ControlBit) -> bool {
        self.0 & (bit as u8) != 0
    }

    #[inline]
    fn set(self, bit: ControlBit) -> Self {
        (self.0 | (bit as u8)).into()
    }

    #[inline]
    fn clear(self, bit: ControlBit) -> Self {
        (self.0 & !(bit as u8)).into()
    }

    #[inline]
    fn flip(self, bit: ControlBit) -> Self {
        (self.0 ^ (bit as u8)).into()
    }

    #[inline]
    fn write_slot_index(&self) -> usize {
        if self.is_set(ControlBit::WriteSlotIndex) {
            1
        } else {
            0
        }
    }

    #[inline]
    fn read_slot_index(&self) -> usize {
        self.flip(ControlBit::WriteSlotIndex).write_slot_index()
    }

    #[inline]
    fn is_slot_initialised(&self, slot: usize) -> bool {
        debug_assert!(slot < 2, "Invalid slot index");
        match slot {
            0 => self.is_set(ControlBit::IsSlot0Initialised),
            1 => self.is_set(ControlBit::IsSlot1Initialised),
            _ => unreachable!("Invalid slot index"),
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

#[cfg(test)]
mod test {
    use super::*;

    impl ControlBlock {
        fn control_bits(&self) -> ControlBits {
            self.control_bits.load(Ordering::SeqCst).into()
        }
    }

    #[test]
    fn reading_swaps_write_slot_index() {
        let control = ControlBlock::default();

        control.write(|slot, initialised| {
            assert_eq!(slot, 0);
            assert!(!initialised);
        });

        control.write(|slot, initialised| {
            assert_eq!(slot, 0);
            assert!(initialised);
        });

        let read_slot = control.read();
        assert_eq!(read_slot, 0);

        control.write(|slot, initialised| {
            assert_eq!(slot, 1);
            assert!(!initialised);
        });

        control.write(|slot, initialised| {
            assert_eq!(slot, 1);
            assert!(initialised);
        });

        let read_slot = control.read();
        assert_eq!(read_slot, 1);

        control.write(|slot, initialised| {
            assert_eq!(slot, 0);
            assert!(initialised);
        });
    }

    #[test]
    fn busy_bit_is_set_whilst_writing() {
        let control = ControlBlock::default();
        assert!(!control.control_bits().is_set(ControlBit::IsWriterBusy));

        control.write(|_, _| {
            assert!(control.control_bits().is_set(ControlBit::IsWriterBusy));
        });

        assert!(!control.control_bits().is_set(ControlBit::IsWriterBusy));
    }

    #[test]
    fn has_new_data_bit_is_set_after_writing() {
        let control = ControlBlock::default();
        assert!(!control.control_bits().is_set(ControlBit::HasNewData));

        control.write(|_, _| {
            assert!(!control.control_bits().is_set(ControlBit::HasNewData));
        });

        assert!(control.control_bits().is_set(ControlBit::HasNewData));
    }
}
