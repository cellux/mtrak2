//! Deterministic scheduler and sandboxed Lua macro runtime for mtrak.
//!
//! The engine advances a single global tick clock, maintains pattern instances
//! with track-local child ownership, and emits timestamped events into a sink.
//! It never talks to MIDI hardware directly, so the whole engine is testable
//! with a fake clock and a collector sink.

#![forbid(unsafe_code)]

pub mod lua;
pub mod macros;
pub mod rng;
pub mod scheduler;
