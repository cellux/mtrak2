//! Core composition model, typed MIDI events, timing, and settings for mtrak.
//!
//! This crate is independent of any MIDI backend, terminal library, or scripting
//! engine. It provides the authoritative in-memory representation of a tracker
//! project: stable pattern IDs, typed events with presence flags, per-pattern
//! timing, and composition-global settings.

#![forbid(unsafe_code)]

pub mod codec;
pub mod composition;
pub mod event;
pub mod modules;
pub mod settings;
pub mod timing;
