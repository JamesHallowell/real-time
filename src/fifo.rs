use {
    crate::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        thread,
    },
    crossbeam_utils::{Backoff, CachePadded},
    std::{alloc, cmp::PartialEq, marker::PhantomData, mem, ops::Deref},
};

/// A handle for writing values to the FIFO.
pub struct Producer<T> {
    buffer: Arc<Buffer<T>>,
}

/// A handle for reading values from the FIFO.
pub struct Consumer<T> {
    buffer: Arc<Buffer<T>>,
}

/// Create a new FIFO with the given capacity.
///
/// This FIFO is optimised for a consumer running on a real-time thread. Elements are not dropped
/// when they are popped, instead they will be dropped when they are overwritten by a later push,
/// or when the buffer is dropped.
pub fn fifo<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let buffer = Arc::new(Buffer::new(capacity));

    (
        Producer {
            buffer: Arc::clone(&buffer),
        },
        Consumer { buffer },
    )
}

unsafe impl<T> Send for Producer<T> where T: Send {}
unsafe impl<T> Send for Consumer<T> where T: Send {}

struct Buffer<T> {
    data: *mut T,
    capacity: usize,
    allocated_capacity: usize,
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
}

impl<T> Producer<T> {
    /// Push a value into the FIFO.
    ///
    /// If the FIFO is full, this method will block until there is space available.
    pub fn push_blocking(&self, mut value: T) {
        let backoff = Backoff::new();

        loop {
            value = match self.push(value) {
                Ok(()) => return,
                Err(value) => value,
            };

            if backoff.is_completed() {
                thread::park();
            } else {
                backoff.snooze();
            }
        }
    }

    /// Push a value into the FIFO.
    ///
    /// If the FIFO is full, this method will not block, and instead returns the value to the caller.
    pub fn push(&self, value: T) -> Result<(), T> {
        let tail = self.buffer.get_tail(Ordering::Relaxed);
        let head = self.buffer.get_head(Ordering::Acquire);

        if self.buffer.is_full(head, tail) {
            return Err(value);
        }

        let ptr = self.buffer.element(tail);
        if usize::from(tail) >= self.buffer.allocated_capacity {
            unsafe { ptr.drop_in_place() };
        }
        unsafe { ptr.write(value) };

        self.buffer.increment_tail(tail, Ordering::Release);
        Ok(())
    }
}

impl<T> Consumer<T> {
    /// Pop a value from the FIFO.
    pub fn pop(&self) -> Option<T>
    where
        T: Copy,
    {
        self.pop_ref().map(|r| *r)
    }

    /// Pop a value from the FIFO by reference.
    ///
    /// This method is useful when the elements in the FIFO do not implement `Copy`.
    pub fn pop_ref(&self) -> Option<PopRef<'_, T>> {
        let tail = self.buffer.get_tail(Ordering::Acquire);
        let head = self.buffer.get_head(Ordering::Relaxed);

        if self.buffer.is_empty(head, tail) {
            return None;
        }

        let value = unsafe { &*self.buffer.element(head) };

        Some(PopRef {
            head,
            value,
            consumer: self,
        })
    }
}

impl<T> Buffer<T> {
    fn new(capacity: usize) -> Self {
        let layout = layout_for::<T>(capacity);
        let buffer = unsafe { alloc::alloc(layout) };
        if buffer.is_null() {
            panic!("failed to allocate buffer");
        }

        Self {
            data: buffer.cast(),
            capacity,
            allocated_capacity: capacity.next_power_of_two(),
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
        }
    }

    fn get_head(&self, ordering: Ordering) -> Head {
        self.head.load(ordering).into()
    }

    fn get_tail(&self, ordering: Ordering) -> Tail {
        self.tail.load(ordering).into()
    }

    fn increment_head(&self, head: Head, ordering: Ordering) {
        let head = self.increment(head);
        self.head.store(head.into(), ordering);
    }

    fn increment_tail(&self, tail: Tail, ordering: Ordering) {
        let tail = self.increment(tail);
        self.tail.store(tail.into(), ordering);
    }

    fn size(&self, head: Head, tail: Tail) -> usize {
        self.index(tail).saturating_sub(self.index(head)) as usize
    }

    fn is_empty(&self, head: Head, tail: Tail) -> bool {
        head == tail
    }

    fn is_full(&self, head: Head, tail: Tail) -> bool {
        self.size(head, tail) == self.capacity
    }

    fn element<Role>(&self, cursor: Cursor<Role>) -> *mut T {
        unsafe { self.data.offset(self.index(cursor)) }
    }

    fn index<Role>(&self, Cursor(cursor, _): Cursor<Role>) -> isize {
        (cursor & (self.allocated_capacity - 1)) as isize
    }

    fn increment<Role>(&self, Cursor(cursor, _): Cursor<Role>) -> Cursor<Role> {
        match cursor.checked_add(1) {
            Some(cursor) => cursor,
            None => (cursor % self.allocated_capacity) + self.allocated_capacity + 1,
        }
        .into()
    }
}

impl<T> Drop for Buffer<T> {
    fn drop(&mut self) {
        let tail = self.get_tail(Ordering::Relaxed).into();
        let n = if tail < self.allocated_capacity {
            tail
        } else {
            self.allocated_capacity
        };

        for i in 0..n {
            let ptr = unsafe { self.data.add(i) };
            unsafe { ptr.drop_in_place() };
        }

        let layout = layout_for::<T>(self.capacity);
        unsafe { alloc::dealloc(self.data.cast(), layout) };
    }
}

fn layout_for<T>(capacity: usize) -> alloc::Layout {
    let bytes = capacity
        .next_power_of_two()
        .checked_mul(mem::size_of::<T>())
        .expect("capacity overflow");

    alloc::Layout::from_size_align(bytes, mem::align_of::<T>()).expect("failed to create layout")
}

/// A reference to a value that has been popped from the FIFO.
pub struct PopRef<'a, T> {
    head: Head,
    value: &'a T,
    consumer: &'a Consumer<T>,
}

impl<T> Deref for PopRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value
    }
}

impl<T> Drop for PopRef<'_, T> {
    fn drop(&mut self) {
        self.consumer
            .buffer
            .increment_head(self.head, Ordering::Release);
    }
}

#[derive(Debug, Copy, Clone)]
struct Cursor<Role>(usize, PhantomData<Role>);

#[derive(Debug, Copy, Clone)]
struct HeadRole;

#[derive(Debug, Copy, Clone)]
struct TailRole;

type Head = Cursor<HeadRole>;

type Tail = Cursor<TailRole>;

impl<RoleA, RoleB> PartialEq<Cursor<RoleA>> for Cursor<RoleB> {
    fn eq(&self, other: &Cursor<RoleA>) -> bool {
        self.0 == other.0
    }
}

impl<Role> From<usize> for Cursor<Role> {
    fn from(value: usize) -> Self {
        Cursor(value, PhantomData)
    }
}

impl<Role> From<Cursor<Role>> for usize {
    fn from(Cursor(cursor, _): Cursor<Role>) -> usize {
        cursor
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn size<T>(producer: &Producer<T>) -> usize {
        producer.buffer.size(
            producer.buffer.get_head(Ordering::Relaxed),
            producer.buffer.get_tail(Ordering::Relaxed),
        )
    }

    #[derive(Debug, Default, Clone)]
    struct DropCounter(Arc<AtomicUsize>);

    impl DropCounter {
        fn count(&self) -> usize {
            self.0.load(Ordering::Relaxed)
        }
    }

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[beady::scenario]
    #[test]
    fn using_a_fifo() {
        'given_an_empty_fifo: {
            let (tx, rx) = fifo::<i32>(3);

            'then_the_fifo_starts_empty: {
                assert_eq!(size(&tx), 0);
            }

            'when_we_try_to_pop_a_value: {
                let value = rx.pop();

                'then_nothing_is_returned: {
                    assert!(value.is_none());
                }
            }

            'when_we_push_a_value: {
                tx.push(5).unwrap();

                'and_when_we_try_to_pop_a_value: {
                    let value = rx.pop();

                    'then_the_popped_value_is_the_one_that_was_pushed: {
                        assert_eq!(value, Some(5));
                    }
                }
            }

            'when_the_fifo_is_at_capacity: {
                tx.push(1).unwrap();
                tx.push(2).unwrap();
                tx.push(3).unwrap();

                'and_when_we_try_to_push_another_value: {
                    let push_result = tx.push(4);

                    'then_the_push_fails: {
                        assert!(push_result.is_err());

                        'and_then_we_get_the_value_we_tried_to_push_back: {
                            assert_eq!(push_result.unwrap_err(), 4);
                        }
                    }
                }

                'and_when_we_pop_a_value: {
                    let value = rx.pop();

                    'then_the_popped_value_is_the_first_one_that_was_pushed: {
                        assert_eq!(value, Some(1));
                    }

                    'and_when_we_push_another_value: {
                        let push_result = tx.push(4);

                        'then_the_push_now_succeeds: {
                            assert!(push_result.is_ok());
                        }
                    }
                }
            }
        }

        'given_a_fifo_whose_elements_do_not_impl_copy: {
            let (tx, rx) = fifo::<String>(2);

            'when_we_push_a_value: {
                tx.push("hello".to_string()).unwrap();

                'and_when_we_pop_a_value_by_ref: {
                    let value_ref = rx.pop_ref();

                    'then_the_pop_succeeds: {
                        assert!(value_ref.is_some());

                        'and_then_the_value_is_correct: {
                            assert_eq!(value_ref.unwrap().as_str(), "hello");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn elements_are_dropped_when_overwritten() {
        let drop_counter = DropCounter::default();
        let (tx, rx) = fifo(3);

        tx.push(drop_counter.clone()).unwrap();
        tx.push(drop_counter.clone()).unwrap();
        tx.push(drop_counter.clone()).unwrap();
        assert_eq!(drop_counter.count(), 0);

        rx.pop_ref();
        assert_eq!(drop_counter.count(), 0);

        tx.push(drop_counter.clone()).unwrap();
        assert_eq!(drop_counter.count(), 0);

        rx.pop_ref();
        assert_eq!(drop_counter.count(), 0);

        tx.push(drop_counter.clone()).unwrap();
        assert_eq!(drop_counter.count(), 1);
    }

    #[test]
    fn elements_are_dropped_when_buffer_is_dropped() {
        let drop_counter = DropCounter::default();
        let (tx, rx) = fifo(3);

        tx.push(drop_counter.clone()).unwrap();
        tx.push(drop_counter.clone()).unwrap();
        tx.push(drop_counter.clone()).unwrap();

        rx.pop_ref();
        assert_eq!(drop_counter.count(), 0);

        tx.push(drop_counter.clone()).unwrap();
        assert_eq!(drop_counter.count(), 0);

        drop((tx, rx));

        assert_eq!(drop_counter.count(), 4);
    }

    #[test]
    fn cursors_wrap_around_and_start_at_the_allocated_capacity() {
        for capacity in 0..1024 {
            let (tx, _rx) = fifo::<i32>(capacity);

            const BEFORE_OVERFLOW: Head = Cursor(usize::MAX - 1, PhantomData);
            const OVERFLOW: Head = Cursor(usize::MAX, PhantomData);

            let prev_head = tx.buffer.increment(BEFORE_OVERFLOW);
            let prev_head_index = tx.buffer.index(prev_head);

            let next_head = tx.buffer.increment(OVERFLOW);
            let next_head_index = tx.buffer.index(next_head);

            assert!(
                next_head.0 >= tx.buffer.allocated_capacity,
                "the wrapped cursor should not reuse the values between [0, allocated_capacity)"
            );
            assert_eq!(
                next_head_index,
                (prev_head_index + 1) % tx.buffer.allocated_capacity as isize
            );
        }
    }
}
