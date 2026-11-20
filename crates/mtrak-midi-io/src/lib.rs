//! Standard MIDI File import/export and tracker project metadata for mtrak.
//!
//! Tracker-specific structure is stored in standards-valid sequencer-specific
//! meta-events (`FF 7F`), alongside a rendered ordinary-MIDI fallback that the
//! loader discards when authoritative metadata is present.

#![forbid(unsafe_code)]

pub mod metadata;
pub mod smf;

pub use metadata::{MetadataError, TrackerProject};
