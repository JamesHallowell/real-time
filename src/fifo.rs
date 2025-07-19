use {
    crate::{
        backoff::Backoff,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    },
    crossbeam_utils::CachePadded,
    std::{alloc, cell::UnsafeCell, cmp::PartialEq, marker::PhantomData, ops::Deref},
};

/// A handle for writing values to the FIFO.
pub struct Producer<T, const N: usize> {
    shared: Arc<Shared<T, N>>,
}

/// A handle for reading values from the FIFO.
pub struct Consumer<T, const N: usize> {
    shared: Arc<Shared<T, N>>,
}

/// Create a new FIFO with the given capacity.
///
/// This FIFO is optimised for a consumer running on a real-time thread.
/// Elements are not dropped when they are popped, instead they will be dropped
/// when they are overwritten by a later push, or when the buffer is dropped.
pub fn fifo<T, const N: usize>() -> (Producer<T, N>, Consumer<T, N>) {
    let shared = Arc::new(Shared::new());

    (
        Producer {
            shared: Arc::clone(&shared),
        },
        Consumer { shared },
    )
}

unsafe impl<T, const N: usize> Send for Producer<T, N> where T: Send {}
unsafe impl<T, const N: usize> Send for Consumer<T, N> where T: Send {}

struct Shared<T, const N: usize> {
    buffer: Buffer<T, N>,
    atomic_head: CachePadded<AtomicHead>,
    cached_tail: CachePadded<CachedTail>,
    cached_head: CachePadded<CachedHead>,
    atomic_tail: CachePadded<AtomicTail>,
}

struct Buffer<T, const N: usize> {
    ptr: *mut T,
}

impl<T, const N: usize> Producer<T, N> {
    /// Push a value into the FIFO.
    ///
    /// If the FIFO is full, this method will block until there is space
    /// available.
    pub fn push_blocking(&self, mut value: T) {
        let backoff = Backoff::default();

        while let Err(value_failed_to_push) = self.push(value) {
            backoff.snooze();
            value = value_failed_to_push;
        }
    }

    /// Push a value into the FIFO.
    ///
    /// If the FIFO is full, this method will not block, and instead returns the
    /// value to the caller.
    pub fn push(&self, value: T) -> Result<(), T> {
        let tail = self.shared.get(Ordering::Relaxed);
        let head = self.shared.get_cached();

        let size = match size(head, tail) {
            size if size < N => size,
            _ => {
                let head = self.shared.get(Ordering::Acquire);
                self.shared.set_cached(head);
                size(head, tail)
            }
        };

        debug_assert!(
            size <= Buffer::<T, N>::SIZE,
            "size ({}) should not be greater than capacity ({})",
            size,
            Buffer::<T, N>::SIZE
        );

        if size == N {
            return Err(value);
        }

        let element = self.shared.buffer.get(tail);
        if self.shared.buffer.has_wrapped(tail) {
            unsafe { element.drop_in_place() };
        }
        unsafe { element.write(value) };

        self.shared.set(advance(tail), Ordering::Release);
        Ok(())
    }
}

impl<T, const N: usize> Consumer<T, N> {
    /// Pop a value from the FIFO.
    pub fn pop(&self) -> Option<T>
    where
        T: Copy,
    {
        self.pop_head_impl().map(|r| *r)
    }

    /// Pop a value from the FIFO by reference.
    ///
    /// This method is useful when the elements in the FIFO do not implement
    /// `Copy`.
    pub fn pop_ref(&mut self) -> Option<PopRef<'_, T, N>> {
        self.pop_head_impl()
    }

    fn pop_head_impl(&self) -> Option<PopRef<'_, T, N>> {
        let head = self.shared.get(Ordering::Relaxed);
        let tail = self.shared.get_cached();

        let size = match size(head, tail) {
            0 => {
                let tail = self.shared.get(Ordering::Acquire);
                self.shared.set_cached(tail);
                size(head, tail)
            }
            size => size,
        };

        debug_assert!(
            size <= Buffer::<T, N>::SIZE,
            "size ({}) should not be greater than capacity ({})",
            size,
            Buffer::<T, N>::SIZE
        );

        if size == 0 {
            return None;
        }

        Some(PopRef {
            head,
            consumer: self,
        })
    }
}

impl<T, const N: usize> Shared<T, N> {
    fn new() -> Self {
        Self {
            buffer: Buffer::new(),
            atomic_head: CachePadded::default(),
            cached_tail: CachePadded::default(),
            cached_head: CachePadded::default(),
            atomic_tail: CachePadded::default(),
        }
    }
}

trait SetCursor<Role> {
    fn set(&self, cursor: Cursor<Role>, ordering: Ordering);
    fn set_cached(&self, cursor: Cursor<Role>);
}

impl<T, const N: usize> SetCursor<HeadRole> for Shared<T, N> {
    #[inline]
    fn set(&self, cursor: Head, ordering: Ordering) {
        self.atomic_head.store(cursor, ordering);
    }

    #[inline]
    fn set_cached(&self, cursor: Head) {
        self.cached_head.set(cursor);
    }
}

impl<T, const N: usize> SetCursor<TailRole> for Shared<T, N> {
    #[inline]
    fn set(&self, cursor: Tail, ordering: Ordering) {
        self.atomic_tail.store(cursor, ordering);
    }

    #[inline]
    fn set_cached(&self, cursor: Tail) {
        self.cached_tail.set(cursor);
    }
}

trait GetCursor<Role> {
    fn get(&self, ordering: Ordering) -> Cursor<Role>;
    fn get_cached(&self) -> Cursor<Role>;
}

impl<T, const N: usize> GetCursor<HeadRole> for Shared<T, N> {
    #[inline]
    fn get(&self, ordering: Ordering) -> Head {
        self.atomic_head.load(ordering)
    }

    #[inline]
    fn get_cached(&self) -> Head {
        self.cached_head.get()
    }
}

impl<T, const N: usize> GetCursor<TailRole> for Shared<T, N> {
    #[inline]
    fn get(&self, ordering: Ordering) -> Tail {
        self.atomic_tail.load(ordering)
    }

    #[inline]
    fn get_cached(&self) -> Tail {
        self.cached_tail.get()
    }
}

impl<T, const N: usize> Drop for Shared<T, N> {
    fn drop(&mut self) {
        let tail: Tail = self.get(Ordering::Relaxed);

        let elements_to_drop = if self.buffer.has_wrapped(tail) {
            Buffer::<T, N>::SIZE
        } else {
            tail.into()
        };

        for i in 0..elements_to_drop {
            let element = self.buffer.at(i);
            unsafe { element.drop_in_place() };
        }
    }
}

impl<T, const N: usize> Buffer<T, N> {
    const SIZE: usize = usize::next_power_of_two(N);

    fn new() -> Self {
        let layout = layout_for::<T>(Self::SIZE);

        let buffer = unsafe { alloc::alloc(layout) };
        if buffer.is_null() {
            panic!("failed to allocate buffer");
        }

        Self { ptr: buffer.cast() }
    }

    #[inline]
    fn at(&self, index: usize) -> *mut T {
        debug_assert!(index < Self::SIZE, "index out of bounds");
        unsafe { self.ptr.add(index) }
    }

    #[inline]
    fn index<Role>(&self, cursor: Cursor<Role>) -> usize {
        index(cursor, Self::SIZE)
    }

    #[inline]
    fn get<Role>(&self, cursor: Cursor<Role>) -> *mut T {
        self.at(self.index(cursor))
    }

    #[inline]
    fn has_wrapped<Role>(&self, Cursor(pos, _): Cursor<Role>) -> bool {
        pos >= Buffer::<T, N>::SIZE
    }
}

impl<T, const N: usize> Drop for Buffer<T, N> {
    fn drop(&mut self) {
        let layout = layout_for::<T>(Self::SIZE);
        unsafe { alloc::dealloc(self.ptr.cast(), layout) };
    }
}

fn layout_for<T>(size: usize) -> alloc::Layout {
    let bytes = size.checked_mul(size_of::<T>()).expect("capacity overflow");
    alloc::Layout::from_size_align(bytes, align_of::<T>()).expect("failed to create layout")
}

/// A reference to a value that has been popped from the FIFO.
pub struct PopRef<'a, T, const N: usize> {
    head: Head,
    consumer: &'a Consumer<T, N>,
}

impl<T, const N: usize> Deref for PopRef<'_, T, N> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        let element = self.consumer.shared.buffer.get(self.head);

        // SAFETY: We have unique access to the value at head for the lifetime of this
        // guard object. Only once it is dropped will the head cursor be advanced.
        unsafe { &*element }
    }
}

impl<T, const N: usize> Drop for PopRef<'_, T, N> {
    fn drop(&mut self) {
        self.consumer
            .shared
            .set(advance(self.head), Ordering::Release);
    }
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone)]
struct Cursor<Role>(usize, PhantomData<Role>);

#[repr(transparent)]
struct AtomicCursor<Role>(AtomicUsize, PhantomData<Role>);

impl<Role> Default for AtomicCursor<Role> {
    fn default() -> Self {
        Self(AtomicUsize::new(0), PhantomData)
    }
}

impl<Role> AtomicCursor<Role> {
    #[inline]
    fn load(&self, ordering: Ordering) -> Cursor<Role> {
        Cursor(self.0.load(ordering), PhantomData)
    }

    #[inline]
    fn store(&self, Cursor(cursor, _): Cursor<Role>, ordering: Ordering) {
        self.0.store(cursor, ordering);
    }
}

#[repr(transparent)]
struct CachedCursor<Role>(UnsafeCell<Cursor<Role>>);

impl<Role> Default for CachedCursor<Role> {
    fn default() -> Self {
        Self(UnsafeCell::new(Cursor(0, PhantomData)))
    }
}

impl<Role> CachedCursor<Role> {
    #[inline]
    fn get(&self) -> Cursor<Role>
    where
        Cursor<Role>: Copy,
    {
        unsafe { *self.0.get() }
    }

    #[inline]
    fn set(&self, cursor: Cursor<Role>) {
        unsafe { *self.0.get() = cursor }
    }
}

#[inline]
fn size(Cursor(head, _): Head, Cursor(tail, _): Tail) -> usize {
    tail - head
}

#[inline]
fn advance<Role>(Cursor(cursor, _): Cursor<Role>) -> Cursor<Role> {
    Cursor(cursor + 1, PhantomData)
}

#[inline]
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

type AtomicHead = AtomicCursor<HeadRole>;

type AtomicTail = AtomicCursor<TailRole>;

type CachedHead = CachedCursor<HeadRole>;

type CachedTail = CachedCursor<TailRole>;

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
    use {
        super::*,
        static_assertions::{assert_impl_all, assert_not_impl_any},
        std::thread,
    };

    assert_impl_all!(Producer<i32, 8>: Send);
    assert_not_impl_any!(Producer<i32, 8>: Sync, Copy, Clone);

    assert_impl_all!(Consumer<i32, 8>: Send);
    assert_not_impl_any!(Consumer<i32, 8>: Sync, Copy, Clone);

    fn get_buffer_size<T, const N: usize>(producer: &Producer<T, N>) -> usize {
        size(
            producer.shared.get(Ordering::Relaxed),
            producer.shared.get(Ordering::Relaxed),
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
        assert_eq!(size(head(0), tail(0)), 0);
        assert_eq!(size(head(0), tail(1)), 1);
        assert_eq!(size(head(0), tail(2)), 2);
        assert_eq!(size(head(0), tail(3)), 3);
        assert_eq!(size(head(1), tail(3)), 2);
        assert_eq!(size(head(2), tail(3)), 1);
        assert_eq!(size(head(3), tail(3)), 0);
    }

    #[test]
    fn advancing_cursors() {
        let cursor = head(0);

        let cursor = advance(cursor);
        assert_eq!(cursor, head(1));

        let cursor = advance(cursor);
        assert_eq!(cursor, head(2));

        let cursor = advance(cursor);
        assert_eq!(cursor, head(3));

        let cursor = advance(cursor);
        assert_eq!(cursor, head(4));

        let cursor = advance(cursor);
        assert_eq!(cursor, head(5));
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

    #[test]
    fn using_a_fifo() {
        let (tx, rx) = fifo::<i32, 3>();
        assert_eq!(get_buffer_size(&tx), 0);

        assert!(rx.pop().is_none());

        tx.push(5).unwrap();

        assert_eq!(rx.pop(), Some(5));

        tx.push(1).unwrap();
        tx.push(2).unwrap();
        tx.push(3).unwrap();

        let push_result = tx.push(4);
        assert_eq!(push_result, Err(4));

        assert_eq!(rx.pop(), Some(1));

        let push_result = tx.push(4);
        assert!(push_result.is_ok());

        let (tx, mut rx) = fifo::<String, 2>();
        tx.push("hello".to_string()).unwrap();

        let value_ref = rx.pop_ref();
        assert!(value_ref.is_some());
        assert_eq!(value_ref.unwrap().as_str(), "hello");
    }

    #[test]
    fn elements_are_dropped_when_overwritten() {
        let drop_counter = DropCounter::default();
        let (tx, mut rx) = fifo::<_, 3>();

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
        let (tx, mut rx) = fifo::<_, 3>();

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
    fn reading_and_writing_on_different_threads() {
        let (writer, reader) = fifo::<_, 12>();

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
