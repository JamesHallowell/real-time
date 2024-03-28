# real-time ⏱️

[![Build](https://github.com/JamesHallowell/real-time/actions/workflows/ci.yml/badge.svg)](https://github.com/JamesHallowell/real-time/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/real-time.svg)](https://crates.io/crates/real-time)
[![Docs.rs](https://docs.rs/real-time/badge.svg)](https://docs.rs/real-time)

**Safely share data with a real-time thread.**

## Overview

This crate provides some tools for sharing data with a real-time thread:

**Shared Values**

Type wrappers that can be used to share values between a real-time thread and another thread, in
a way that is real-time safe.

They use the same algorithms as `RealtimeObject`
from [FAbian's Realtime Box o' Tricks](https://github.com/hogliux/farbot), that
was [presented at Meeting C++ 2019](https://www.youtube.com/watch?v=ndeN983j_GQ).

- [`RealtimeReader`], for reading from a shared value on a real-time thread.
- [`RealtimeWriter`], for writing to a shared value on a real-time thread.

**FIFOs**

- [`fifo`], a lock-free single-producer, single-consumer FIFO that is optimised for a real-time consumer.

[`RealtimeReader`]: https://docs.rs/real-time/latest/real_time/reader/struct.RealtimeReader.html

[`RealtimeWriter`]: https://docs.rs/real-time/latest/real_time/writer/struct.RealtimeWriter.html

[`fifo`]: https://docs.rs/real-time/latest/real_time/fifo/fn.fifo.html

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
real-time = "0.5"
```

## License

Licensed under either of

* Apache License, Version 2.0
  ([LICENSE-APACHE](LICENSE-APACHE))
* MIT license
  ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
