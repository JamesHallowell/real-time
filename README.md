# real-time ⏱️

[![Build](https://github.com/JamesHallowell/real-time/actions/workflows/ci.yml/badge.svg)](https://github.com/JamesHallowell/real-time/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/real-time.svg)](https://crates.io/crates/real-time)
[![Docs.rs](https://docs.rs/real-time/badge.svg)](https://docs.rs/real-time)

**Safely share data with a real-time thread.**

## Overview

This crate provides some tools for sharing data with a real-time thread.

### [`real_time::readable`]

A shared value that can be read on a real-time thread.

```rust
fn main() {
    let (writer, reader) = real_time::readable(SynthParameters::default());
}
```

### [`real_time::writable`]

A shared value that can be written to from a real-time thread.

```rust
fn main() {
    let (writer, reader) = real_time::writable(AudioPlaybackStats::default());
}
```

### [`real_time::fifo`]

A bounded, lock-free, single-producer, single consumer FIFO that is optimised for a real-time consumer. Values sent will
be
reclaimed and dropped by the producer once the real-time consumer has finished with them.

```rust
fn main() {
    let (producer, consumer) = real_time::fifo::<MidiMessage, 16>();
}
```

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
real-time = "0.7"
```

## Implementation

[`real_time::readable`] and [`real_time::writable`] use the same algorithms as `RealtimeObject`
from [FAbian's Realtime Box o' Tricks](https://github.com/hogliux/farbot), that
was [presented at Meeting C++ 2019](https://www.youtube.com/watch?v=ndeN983j_GQ).

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

[`real_time::readable`]: https://docs.rs/real-time/latest/real_time/reader/fn.readable.html

[`real_time::writable`]: https://docs.rs/real-time/latest/real_time/writer/fn.writable.html

[`real_time::fifo`]: https://docs.rs/real-time/latest/real_time/fifo/fn.fifo.html
