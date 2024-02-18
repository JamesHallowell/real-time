#[cfg(not(loom))]
pub use std::hint::spin_loop;

#[cfg(loom)]
pub use loom::thread::yield_now as spin_loop;
