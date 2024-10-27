use {
    crate::{
        backoff::Backoff,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    },
    crossbeam_utils::CachePadded,
    std::{alloc, cmp::PartialEq, marker::PhantomData, ops::Deref},
};

/// A handle for writing values to the FIFO.
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
}

/// A handle for reading values from the FIFO.
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
}

/// Create a new FIFO with the given capacity.
///
/// This FIFO is optimised for a consumer running on a real-time thread. Elements are not dropped
/// when they are popped, instead they will be dropped when they are overwritten by a later push,
/// or when the buffer is dropped.
pub fn fifo<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let shared = Arc::new(Shared::new(capacity));

    (
        Producer {
            shared: Arc::clone(&shared),
        },
        Consumer { shared },
    )
}

unsafe impl<T> Send for Producer<T> where T: Send {}
unsafe impl<T> Send for Consumer<T> where T: Send {}

struct Shared<T> {
    capacity: usize,
    buffer: Buffer<T>,
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
}

struct Buffer<T> {
    ptr: *mut T,
    len: usize,
}

impl<T> Producer<T> {
    /// Push a value into the FIFO.
    ///
    /// If the FIFO is full, this method will block until there is space available.
    pub fn push_blocking(&self, mut value: T) {
        let backoff = Backoff::default();

        while let Err(value_failed_to_push) = self.push(value) {
            backoff.snooze();
            value = value_failed_to_push;
        }
    }

    /// Push a value into the FIFO.
    ///
    /// If the FIFO is full, this method will not block, and instead returns the value to the caller.
    pub fn push(&self, value: T) -> Result<(), T> {
        let tail = self.shared.get(Ordering::Relaxed);
        let head = self.shared.get(Ordering::Acquire);

        let size = self.shared.size(head, tail);
        assert!(
            size <= self.shared.capacity,
            "size ({}) should not be greater than capacity ({})",
            size,
            self.shared.capacity
        );

        if size == self.shared.capacity {
            return Err(value);
        }

        let element = self.shared.element(tail);
        if self.shared.has_wrapped_around(tail) {
            unsafe { element.drop_in_place() };
        }
        unsafe { element.write(value) };

        self.shared.advance(tail, Ordering::Release);
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
        let head = self.shared.get(Ordering::Relaxed);
        let tail = self.shared.get(Ordering::Acquire);

        let size = self.shared.size(head, tail);
        assert!(
            size <= self.shared.capacity,
            "size ({}) should not be greater than capacity ({})",
            size,
            self.shared.capacity
        );

        if size == 0 {
            return None;
        }

        let element = self.shared.element(head);
        let value = unsafe { &*element };

        Some(PopRef {
            head,
            value,
            consumer: self,
        })
    }
}

impl<T> Shared<T> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            buffer: Buffer::new(capacity),
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
        }
    }

    fn size(&self, head: Head, tail: Tail) -> usize {
        size(head, tail, self.buffer.len)
    }

    fn index<Role>(&self, cursor: Cursor<Role>) -> usize {
        index(cursor, self.buffer.len)
    }

    fn element<Role>(&self, cursor: Cursor<Role>) -> *mut T {
        self.buffer.at(self.index(cursor))
    }

    fn has_wrapped_around<Role>(&self, Cursor(pos, _): Cursor<Role>) -> bool {
        pos >= self.buffer.len
    }

    fn advance<Role>(&self, cursor: Cursor<Role>, ordering: Ordering)
    where
        Self: SetCursor<Role>,
    {
        self.set(advance(cursor, self.buffer.len), ordering);
    }
}

trait SetCursor<Role> {
    fn set(&self, cursor: Cursor<Role>, ordering: Ordering);
}

impl<T> SetCursor<HeadRole> for Shared<T> {
    fn set(&self, head: Head, ordering: Ordering) {
        self.head.store(head.into(), ordering);
    }
}

impl<T> SetCursor<TailRole> for Shared<T> {
    fn set(&self, tail: Tail, ordering: Ordering) {
        self.tail.store(tail.into(), ordering);
    }
}

trait GetCursor<Role> {
    fn get(&self, ordering: Ordering) -> Cursor<Role>;
}

impl<T> GetCursor<HeadRole> for Shared<T> {
    fn get(&self, ordering: Ordering) -> Head {
        self.head.load(ordering).into()
    }
}

impl<T> GetCursor<TailRole> for Shared<T> {
    fn get(&self, ordering: Ordering) -> Tail {
        self.tail.load(ordering).into()
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        let tail: Tail = self.get(Ordering::Relaxed);

        let elements_to_drop = if self.has_wrapped_around(tail) {
            self.buffer.len
        } else {
            tail.into()
        };

        for i in 0..elements_to_drop {
            let element = self.buffer.at(i);
            unsafe { element.drop_in_place() };
        }
    }
}

impl<T> Buffer<T> {
    fn new(size: usize) -> Self {
        let size = size.next_power_of_two();
        let layout = layout_for::<T>(size);

        let buffer = unsafe { alloc::alloc(layout) };
        if buffer.is_null() {
            panic!("failed to allocate buffer");
        }

        Self {
            ptr: buffer.cast(),
            len: size,
        }
    }

    fn at(&self, index: usize) -> *mut T {
        debug_assert!(index < self.len, "index out of bounds");
        unsafe { self.ptr.add(index) }
    }
}

impl<T> Drop for Buffer<T> {
    fn drop(&mut self) {
        let layout = layout_for::<T>(self.len);
        unsafe { alloc::dealloc(self.ptr.cast(), layout) };
    }
}

fn layout_for<T>(size: usize) -> alloc::Layout {
    let bytes = size.checked_mul(size_of::<T>()).expect("capacity overflow");
    alloc::Layout::from_size_align(bytes, align_of::<T>()).expect("failed to create layout")
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
        self.consumer.shared.advance(self.head, Ordering::Release);
    }
}

#[derive(Debug, Copy, Clone)]
struct Cursor<Role>(usize, PhantomData<Role>);

fn size(head: Head, tail: Tail, size: usize) -> usize {
    if head == tail {
        return 0;
    }

    let head = index(head, size);
    let tail = index(tail, size);

    if head < tail {
        tail - head
    } else {
        size - head + tail
    }
}

fn advance<Role>(Cursor(cursor, _): Cursor<Role>, size: usize) -> Cursor<Role> {
    match cursor.checked_add(1) {
        Some(cursor) => cursor,
        None => (cursor % size) + size + 1,
    }
    .into()
}

fn index<Role>(Cursor(cursor, _): Cursor<Role>, size: usize) -> usize {
    debug_assert!(
        size.is_power_of_two(),
        "size must be a power of two, got {size:?}",
    );

    cursor & (size - 1)
}

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

impl<RoleA, RoleB> PartialOrd<Cursor<RoleA>> for Cursor<RoleB> {
    fn partial_cmp(&self, other: &Cursor<RoleA>) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
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
    use {super::*, std::thread};

    fn get_buffer_size<T>(producer: &Producer<T>) -> usize {
        size(
            producer.shared.get(Ordering::Relaxed),
            producer.shared.get(Ordering::Relaxed),
            producer.shared.buffer.len,
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

    fn head(pos: usize) -> Head {
        Cursor(pos, PhantomData)
    }

    fn tail(pos: usize) -> Tail {
        Cursor(pos, PhantomData)
    }

    #[test]
    fn querying_size() {
        assert_eq!(size(head(0), tail(0), 4), 0);
        assert_eq!(size(head(0), tail(1), 4), 1);
        assert_eq!(size(head(0), tail(2), 4), 2);
        assert_eq!(size(head(0), tail(3), 4), 3);
        assert_eq!(size(head(1), tail(3), 4), 2);
        assert_eq!(size(head(2), tail(3), 4), 1);
        assert_eq!(size(head(3), tail(3), 4), 0);
    }

    #[test]
    fn advancing_cursors() {
        let cursor = head(0);

        let cursor = advance(cursor, 4);
        assert_eq!(cursor, head(1));

        let cursor = advance(cursor, 4);
        assert_eq!(cursor, head(2));

        let cursor = advance(cursor, 4);
        assert_eq!(cursor, head(3));

        let cursor = advance(cursor, 4);
        assert_eq!(cursor, head(4));

        let cursor = advance(cursor, 4);
        assert_eq!(cursor, head(5));
    }

    #[test]
    fn cursor_overflow() {
        for capacity in [2, 4, 8, 16, 32, 64, 128, 256, 512, 1024] {
            let head = head(usize::MAX - capacity);
            let tail = tail(usize::MAX);

            assert_eq!(size(head, tail, capacity), capacity);

            let head = advance(head, capacity);

            assert_eq!(size(head, tail, capacity), capacity - 1);

            assert!(head < tail);
            let tail = advance(tail, capacity);
            assert!(tail < head);

            assert_eq!(size(head, tail, capacity), capacity);
        }
    }

    #[test]
    fn cursor_to_index() {
        assert_eq!(index(head(0), 4), 0);
        assert_eq!(index(head(1), 4), 1);
        assert_eq!(index(head(2), 4), 2);
        assert_eq!(index(head(3), 4), 3);
        assert_eq!(index(head(4), 4), 0);
        assert_eq!(index(head(5), 4), 1);
        assert_eq!(index(head(6), 4), 2);
        assert_eq!(index(head(7), 4), 3);
        assert_eq!(index(head(8), 4), 0);
    }

    #[beady::scenario]
    #[test]
    fn using_a_fifo() {
        'given_an_empty_fifo: {
            let (tx, rx) = fifo::<i32>(3);

            'then_the_fifo_starts_empty: {
                assert_eq!(get_buffer_size(&tx), 0);
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
        const BEFORE_OVERFLOW: Tail = Cursor(usize::MAX - 1, PhantomData);
        const OVERFLOW: Tail = Cursor(usize::MAX, PhantomData);

        for capacity in (0..=1024).map(|capacity: usize| capacity.next_power_of_two()) {
            let prev_tail = advance(BEFORE_OVERFLOW, capacity);
            let prev_tail_index = index(prev_tail, capacity);

            let next_tail = advance(OVERFLOW, capacity);
            let next_tail_index = index(next_tail, capacity);

            // The wrapped cursor should not reuse the values between [0, allocated_capacity)
            // because they are used to determine if elements can be safely dropped.
            assert!(
                next_tail.0 >= capacity,
                "the wrapped cursor should not reuse the values between [0, allocated_capacity)"
            );
            assert_eq!(next_tail_index, (prev_tail_index + 1) % capacity);
        }
    }

    #[test]
    fn reading_and_writing_on_different_threads() {
        let (writer, reader) = fifo(12);

        #[cfg(miri)]
        const NUM_WRITES: usize = 128;

        #[cfg(not(miri))]
        const NUM_WRITES: usize = 1_000_000;

        thread::spawn({
            move || {
                for value in 1..=NUM_WRITES {
                    writer.push_blocking(value);
                }
            }
        });

        let mut last = None;
        while last != Some(NUM_WRITES) {
            match reader.pop() {
                Some(value) => {
                    if let Some(last) = last {
                        assert_eq!(last + 1, value, "values should be popped in order");
                    }
                    last = Some(value);
                }
                None => thread::yield_now(),
            }
        }
    }
}
