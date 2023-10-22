#![warn(missing_docs)]

//! Safely share data with a real-time thread.

/// Read shared data on the real-time thread.
pub mod reader;

mod sync;
