# real-time

[![Build](https://github.com/JamesHallowell/real-time/actions/workflows/ci.yml/badge.svg)](https://github.com/JamesHallowell/real-time/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/real-time.svg)](https://crates.io/crates/real-time)
[![Docs.rs](https://docs.rs/real-time/badge.svg)](https://docs.rs/real-time)

**Safely share data with a real-time thread.**

## Overview

This is a port of some of the algorithms used by `RealtimeObject`
from [FAbian's Realtime Box o' Tricks](https://github.com/hogliux/farbot), that was
[presented at Meeting C++ 2019](https://www.youtube.com/watch?v=ndeN983j_GQ).

It allows data to be shared safely between
a single real-time thread and multiple other threads, without blocking the real-time thread.

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
real-time = "0.3"
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
