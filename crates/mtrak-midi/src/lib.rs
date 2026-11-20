//! Portable MIDI backend abstraction for mtrak.
//!
//! The engine emits timestamped events into a sink. The real implementation
//! wraps `midir` behind the optional `midir-backend` feature; a [`MockBackend`]
//! keeps the engine testable without any MIDI hardware.

#![forbid(unsafe_code)]

pub mod backend;
#[cfg(feature = "midir-backend")]
pub mod midir_backend;

pub use backend::{
    ingest_message, EngineSink, InputMessage, InputSource, MidiBackend, MidiError, MidiOutput,
    MockBackend, OutputManager, PortInfo,
};
