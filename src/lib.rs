#![warn(missing_docs)]

//! Safely share data with a real-time thread.

/// Read shared data on the real-time thread.
pub mod reader;

/// Write shared data on the real-time thread.
pub mod writer;

mod sync;

type PhantomUnsync = std::marker::PhantomData<std::cell::Cell<()>>;
