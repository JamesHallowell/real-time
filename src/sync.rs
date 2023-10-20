#[cfg(not(loom))]
pub use std::sync::*;

#[cfg(loom)]
pub use loom::sync::*;
