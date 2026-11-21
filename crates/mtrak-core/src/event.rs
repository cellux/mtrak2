//! Typed MIDI and tracker event model.
//!
//! Every slot in a pattern holds exactly one [`Event`]. Channel-voice messages
//! keep their fields as [`Option<u8>`] so the model can distinguish an omitted
//! (inherited) field from an explicitly supplied `00` byte — a distinction the
//! tracker editor relies on and that incomplete events cannot erase.
//!
//! Meta-events preserve their typed meaning *and* their raw payload, so import
//! and export can round-trip content the tracker has no specialized editor for.

use crate::codec::{read_vlq, write_vlq};

/// Errors produced while validating, encoding, or decoding an [`Event`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventError {
    /// The event is missing a required field and cannot be emitted.
    #[error("event is incomplete and cannot be emitted as MIDI")]
    Incomplete,
    /// The input ended in the middle of an event.
    #[error("unexpected end of input while decoding event")]
    UnexpectedEof,
    /// A status byte outside the supported ranges was encountered.
    #[error("invalid status byte 0x{0:02X}")]
    InvalidStatus(u8),
    /// The event is not representable in the requested encoding.
    #[error("event is not representable as {0}")]
    Unsupported(&'static str),
    /// A variable-length quantity was malformed.
    #[error("malformed variable-length quantity")]
    InvalidVlq,
    /// A channel number outside `0..=15` was supplied.
    #[error("channel out of range: {0}")]
    ChannelOutOfRange(u8),
    /// The byte sequence is not a valid MIDI/SMF event form.
    #[error("invalid event bytes: {0}")]
    InvalidWire(String),
}

/// The seven channel-voice message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    /// Note off (`0x8n`).
    NoteOff,
    /// Note on (`0x9n`).
    NoteOn,
    /// Polyphonic key pressure / aftertouch (`0xAn`).
    PolyAftertouch,
    /// Control change (`0xBn`).
    ControlChange,
    /// Program change (`0xCn`).
    ProgramChange,
    /// Channel pressure / aftertouch (`0xDn`).
    ChannelAftertouch,
    /// Pitch bend (`0xEn`).
    PitchBend,
}

impl ChannelKind {
    /// The high nibble of the status byte for this message type.
    pub fn status_nibble(self) -> u8 {
        match self {
            ChannelKind::NoteOff => 0x8,
            ChannelKind::NoteOn => 0x9,
            ChannelKind::PolyAftertouch => 0xA,
            ChannelKind::ControlChange => 0xB,
            ChannelKind::ProgramChange => 0xC,
            ChannelKind::ChannelAftertouch => 0xD,
            ChannelKind::PitchBend => 0xE,
        }
    }

    /// Builds a kind from the high nibble of a status byte.
    pub fn from_status_nibble(nibble: u8) -> Option<ChannelKind> {
        Some(match nibble {
            0x8 => ChannelKind::NoteOff,
            0x9 => ChannelKind::NoteOn,
            0xA => ChannelKind::PolyAftertouch,
            0xB => ChannelKind::ControlChange,
            0xC => ChannelKind::ProgramChange,
            0xD => ChannelKind::ChannelAftertouch,
            0xE => ChannelKind::PitchBend,
            _ => return None,
        })
    }

    /// The number of data bytes that follow the status byte.
    pub fn data_len(self) -> usize {
        match self {
            ChannelKind::ProgramChange | ChannelKind::ChannelAftertouch => 1,
            _ => 2,
        }
    }

    /// A short human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            ChannelKind::NoteOff => "note-off",
            ChannelKind::NoteOn => "note-on",
            ChannelKind::PolyAftertouch => "poly-aftertouch",
            ChannelKind::ControlChange => "control-change",
            ChannelKind::ProgramChange => "program-change",
            ChannelKind::ChannelAftertouch => "channel-aftertouch",
            ChannelKind::PitchBend => "pitch-bend",
        }
    }
}

/// A channel-voice message with per-field presence flags.
///
/// `None` means the field is omitted and may be inherited from the track-local
/// defaults; `Some(0)` is an explicit zero byte. The two are never conflated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelMessage {
    /// The message type (determines the status high nibble).
    pub kind: ChannelKind,
    /// The MIDI channel (`0..=15`), or `None` to inherit.
    pub channel: Option<u8>,
    /// The first data byte, or `None` to inherit.
    pub data1: Option<u8>,
    /// The second data byte (only for two-data-byte kinds), or `None`.
    pub data2: Option<u8>,
}

impl ChannelMessage {
    /// Creates a message with all fields omitted.
    pub fn new(kind: ChannelKind) -> Self {
        ChannelMessage {
            kind,
            channel: None,
            data1: None,
            data2: None,
        }
    }

    /// Creates a fully specified message.
    pub fn with_data(kind: ChannelKind, channel: u8, data1: u8, data2: Option<u8>) -> Self {
        ChannelMessage {
            kind,
            channel: Some(channel),
            data1: Some(data1),
            data2,
        }
    }

    /// Whether every required field is present and in range.
    pub fn is_complete(&self) -> bool {
        if !matches!(self.channel, Some(c) if c < 16) {
            return false;
        }
        if self.data1.is_none() {
            return false;
        }
        if self.kind.data_len() == 2 && self.data2.is_none() {
            return false;
        }
        true
    }

    /// Encodes the wire/SMF body bytes, or [`EventError::Incomplete`].
    pub fn encode(&self) -> Result<Vec<u8>, EventError> {
        let Some(channel) = self.channel else {
            return Err(EventError::Incomplete);
        };
        if channel > 15 {
            return Err(EventError::ChannelOutOfRange(channel));
        }
        let Some(data1) = self.data1 else {
            return Err(EventError::Incomplete);
        };
        let mut out = Vec::with_capacity(3);
        out.push((self.kind.status_nibble() << 4) | channel);
        out.push(data1);
        if self.kind.data_len() == 2 {
            let Some(data2) = self.data2 else {
                return Err(EventError::Incomplete);
            };
            out.push(data2);
        }
        Ok(out)
    }

    /// Decodes a channel message from a status byte and its data bytes.
    pub fn decode(status: u8, data: &[u8]) -> Result<ChannelMessage, EventError> {
        let kind = ChannelKind::from_status_nibble(status >> 4)
            .ok_or(EventError::InvalidStatus(status))?;
        let need = kind.data_len();
        if data.len() < need {
            return Err(EventError::UnexpectedEof);
        }
        Ok(ChannelMessage {
            kind,
            channel: Some(status & 0x0F),
            data1: Some(data[0]),
            data2: if need == 2 { Some(data[1]) } else { None },
        })
    }

    /// Renders the message as inline hexadecimal, using `·` for absent nibbles.
    ///
    /// The length is `2 + 2 * data_len` characters; the TUI pads this to its
    /// fixed column width.
    pub fn inline_hex(&self) -> String {
        let mut out = String::new();
        match self.channel {
            Some(c) => out.push_str(&format!("{:X}{:X}", self.kind.status_nibble(), c & 0x0F)),
            None => out.push_str("··"),
        }
        push_hex_byte(&mut out, self.data1);
        if self.kind.data_len() == 2 {
            push_hex_byte(&mut out, self.data2);
        }
        out
    }
}

fn push_hex_byte(out: &mut String, byte: Option<u8>) {
    match byte {
        Some(b) => out.push_str(&format!("{b:02X}")),
        None => out.push_str("··"),
    }
}

/// The specific kind of a text-based meta-event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextKind {
    /// `0x01` — arbitrary text.
    Text,
    /// `0x02` — copyright notice.
    Copyright,
    /// `0x03` — track/sequence name.
    TrackName,
    /// `0x04` — instrument name.
    InstrumentName,
    /// `0x05` — lyric.
    Lyric,
    /// `0x06` — marker.
    Marker,
    /// `0x07` — cue point.
    CuePoint,
    /// `0x08` — program name.
    ProgramName,
    /// `0x09` — device name.
    DeviceName,
}

impl TextKind {
    /// The SMF meta type byte for this text kind.
    pub fn type_byte(self) -> u8 {
        match self {
            TextKind::Text => 0x01,
            TextKind::Copyright => 0x02,
            TextKind::TrackName => 0x03,
            TextKind::InstrumentName => 0x04,
            TextKind::Lyric => 0x05,
            TextKind::Marker => 0x06,
            TextKind::CuePoint => 0x07,
            TextKind::ProgramName => 0x08,
            TextKind::DeviceName => 0x09,
        }
    }

    /// Maps an SMF meta type byte to a text kind, if it is one.
    pub fn from_type_byte(byte: u8) -> Option<TextKind> {
        Some(match byte {
            0x01 => TextKind::Text,
            0x02 => TextKind::Copyright,
            0x03 => TextKind::TrackName,
            0x04 => TextKind::InstrumentName,
            0x05 => TextKind::Lyric,
            0x06 => TextKind::Marker,
            0x07 => TextKind::CuePoint,
            0x08 => TextKind::ProgramName,
            0x09 => TextKind::DeviceName,
            _ => return None,
        })
    }
}

/// An SMF time-signature meta-event (`FF 58 04`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeSignature {
    /// Numerator of the signature.
    pub numerator: u8,
    /// Log2 of the denominator (e.g. `2` means a quarter note).
    pub denominator_pow2: u8,
    /// MIDI clocks per metronome click.
    pub clocks_per_click: u8,
    /// Number of notated 32nd notes per MIDI quarter note.
    pub notated_32nds_per_quarter: u8,
}

/// An SMF key-signature meta-event (`FF 59 02`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySignature {
    /// Number of sharps (positive) or flats (negative), in `-7..=7`.
    pub sharps_flats: i8,
    /// Whether the key is minor.
    pub is_minor: bool,
}

/// An SMF SMPTE offset meta-event (`FF 54 05`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SmpteOffset(pub [u8; 5]);

/// A MIDI-file meta-event with typed variants and raw payload preservation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MetaEvent {
    /// `FF 00 02` — sequence number.
    SequenceNumber(u16),
    /// `FF 01..09` — text-style events.
    Text {
        /// Which text-style event this is.
        kind: TextKind,
        /// Raw text bytes (not required to be UTF-8).
        bytes: Vec<u8>,
    },
    /// `FF 20 01` — channel prefix.
    ChannelPrefix(u8),
    /// `FF 2F 00` — end of track.
    EndOfTrack,
    /// `FF 51 03` — tempo in microseconds per quarter note.
    Tempo(u32),
    /// `FF 54 05` — SMPTE offset.
    SmpteOffset(SmpteOffset),
    /// `FF 58 04` — time signature.
    TimeSignature(TimeSignature),
    /// `FF 59 02` — key signature.
    KeySignature(KeySignature),
    /// `FF 7F` — sequencer-specific payload.
    SequencerSpecific(Vec<u8>),
    /// Any other meta-event, preserved verbatim.
    Unknown {
        /// The meta type byte.
        kind: u8,
        /// The raw payload.
        data: Vec<u8>,
    },
}

impl MetaEvent {
    /// Creates a text meta-event from raw bytes.
    pub fn text(kind: TextKind, bytes: impl Into<Vec<u8>>) -> Self {
        MetaEvent::Text {
            kind,
            bytes: bytes.into(),
        }
    }

    /// The meta type byte.
    pub fn type_byte(&self) -> u8 {
        match self {
            MetaEvent::SequenceNumber(_) => 0x00,
            MetaEvent::Text { kind, .. } => kind.type_byte(),
            MetaEvent::ChannelPrefix(_) => 0x20,
            MetaEvent::EndOfTrack => 0x2F,
            MetaEvent::Tempo(_) => 0x51,
            MetaEvent::SmpteOffset(_) => 0x54,
            MetaEvent::TimeSignature(_) => 0x58,
            MetaEvent::KeySignature(_) => 0x59,
            MetaEvent::SequencerSpecific(_) => 0x7F,
            MetaEvent::Unknown { kind, .. } => *kind,
        }
    }

    /// The raw payload bytes that follow the type byte.
    pub fn data_bytes(&self) -> Vec<u8> {
        match self {
            MetaEvent::SequenceNumber(n) => n.to_be_bytes().to_vec(),
            MetaEvent::Text { bytes, .. } => bytes.clone(),
            MetaEvent::ChannelPrefix(c) => vec![*c],
            MetaEvent::EndOfTrack => Vec::new(),
            MetaEvent::Tempo(mpqn) => {
                let b = mpqn.to_be_bytes();
                vec![b[1], b[2], b[3]]
            }
            MetaEvent::SmpteOffset(SmpteOffset(bytes)) => bytes.to_vec(),
            MetaEvent::TimeSignature(ts) => vec![
                ts.numerator,
                ts.denominator_pow2,
                ts.clocks_per_click,
                ts.notated_32nds_per_quarter,
            ],
            MetaEvent::KeySignature(ks) => {
                vec![ks.sharps_flats as u8, u8::from(ks.is_minor)]
            }
            MetaEvent::SequencerSpecific(data) => data.clone(),
            MetaEvent::Unknown { data, .. } => data.clone(),
        }
    }

    /// Reconstructs a meta-event from its type byte and raw payload.
    ///
    /// Typed variants are only produced when the payload has the exact expected
    /// length; anything else falls back to [`MetaEvent::Unknown`], which keeps
    /// the bytes intact.
    pub fn from_parts(type_byte: u8, data: Vec<u8>) -> MetaEvent {
        match type_byte {
            0x00 if data.len() == 2 => {
                MetaEvent::SequenceNumber(u16::from_be_bytes([data[0], data[1]]))
            }
            0x20 if data.len() == 1 => MetaEvent::ChannelPrefix(data[0]),
            0x2F if data.is_empty() => MetaEvent::EndOfTrack,
            0x51 if data.len() == 3 => {
                MetaEvent::Tempo(u32::from_be_bytes([0, data[0], data[1], data[2]]))
            }
            0x54 if data.len() == 5 => {
                MetaEvent::SmpteOffset(SmpteOffset([data[0], data[1], data[2], data[3], data[4]]))
            }
            0x58 if data.len() == 4 => MetaEvent::TimeSignature(TimeSignature {
                numerator: data[0],
                denominator_pow2: data[1],
                clocks_per_click: data[2],
                notated_32nds_per_quarter: data[3],
            }),
            0x59 if data.len() == 2 => MetaEvent::KeySignature(KeySignature {
                sharps_flats: data[0] as i8,
                is_minor: data[1] != 0,
            }),
            0x7F => MetaEvent::SequencerSpecific(data),
            other => match TextKind::from_type_byte(other) {
                Some(kind) => MetaEvent::Text { kind, bytes: data },
                None => MetaEvent::Unknown { kind: other, data },
            },
        }
    }

    /// Encodes the full SMF representation, including `FF`, type, and length.
    pub fn encode(&self) -> Vec<u8> {
        let data = self.data_bytes();
        let mut out = vec![0xFF, self.type_byte()];
        write_vlq(data.len() as u32, &mut out);
        out.extend_from_slice(&data);
        out
    }
}

/// The class of an event for editing and display purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventClass {
    /// An empty slot.
    Empty,
    /// Editable inline as six hexadecimal character positions.
    Inline,
    /// Shown as a marker and edited through the full event editor.
    Complex,
}

/// Tracker-specific macro identifier ranges (see decision D-011).
pub mod macro_id {
    /// The project setup module; not usable as a composition macro.
    pub const SETUP: u8 = 0x00;
    /// The built-in pattern-trigger macro.
    pub const BUILTIN_TRIGGER: u8 = 0x01;
    /// The built-in Q8.8 tempo shorthand macro.
    pub const BUILTIN_TEMPO: u8 = 0x02;
    /// The lowest built-in macro ID.
    pub const BUILTIN_MIN: u8 = 0x01;
    /// The highest built-in macro ID.
    pub const BUILTIN_MAX: u8 = 0x3F;
    /// The lowest user macro ID.
    pub const USER_MIN: u8 = 0x40;
    /// The highest user macro ID.
    pub const USER_MAX: u8 = 0x7F;

    /// Whether `id` is in the built-in range.
    pub fn is_builtin(id: u8) -> bool {
        (BUILTIN_MIN..=BUILTIN_MAX).contains(&id)
    }

    /// Whether `id` is in the user-authorable range.
    pub fn is_user(id: u8) -> bool {
        (USER_MIN..=USER_MAX).contains(&id)
    }

    /// Whether `id` is reserved (the setup module or the `80..=FF` overlap).
    pub fn is_reserved(id: u8) -> bool {
        id == SETUP || id >= 0x80
    }

    /// Whether `id` names a built-in that v1 defines.
    pub fn is_defined_builtin(id: u8) -> bool {
        id == BUILTIN_TRIGGER || id == BUILTIN_TEMPO
    }
}

/// A tracker macro invocation with its two uniform argument bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacroCall {
    /// The macro ID (`00`–`FF`; range validity is enforced elsewhere).
    pub id: u8,
    /// The first raw argument byte.
    pub arg1: u8,
    /// The second raw argument byte.
    pub arg2: u8,
}

impl MacroCall {
    /// Creates a macro call.
    pub fn new(id: u8, arg1: u8, arg2: u8) -> Self {
        MacroCall { id, arg1, arg2 }
    }

    /// The two raw argument bytes, in order.
    pub fn args(&self) -> [u8; 2] {
        [self.arg1, self.arg2]
    }

    /// The three compact bytes as they appear inline.
    pub fn bytes(&self) -> [u8; 3] {
        [self.id, self.arg1, self.arg2]
    }
}

/// A single tracker event.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum Event {
    /// An empty slot.
    #[default]
    Empty,
    /// A channel-voice message.
    Channel(ChannelMessage),
    /// A system-exclusive message; the payload excludes the `F0`/`F7` markers.
    SysEx(Vec<u8>),
    /// A MIDI-file meta-event.
    Meta(MetaEvent),
    /// A valid Standard MIDI File Escape event (`F7` framing); file-only.
    SmfEscape(Vec<u8>),
    /// A tracker macro call.
    Macro(MacroCall),
    /// A validated generic live wire-MIDI byte sequence.
    Wire(Vec<u8>),
}

impl Event {
    /// Whether this is an empty slot.
    pub fn is_empty(&self) -> bool {
        matches!(self, Event::Empty)
    }

    /// The editing/display class of this event.
    pub fn class(&self) -> EventClass {
        match self {
            Event::Empty => EventClass::Empty,
            Event::Channel(_) | Event::Macro(_) => EventClass::Inline,
            Event::SysEx(_) | Event::Meta(_) | Event::SmfEscape(_) | Event::Wire(_) => {
                EventClass::Complex
            }
        }
    }

    /// Whether this event is a complete, emittable MIDI message.
    pub fn is_complete(&self) -> bool {
        match self {
            Event::Empty => false,
            Event::Channel(m) => m.is_complete(),
            Event::SysEx(_) | Event::Wire(_) => true,
            Event::Meta(_) | Event::SmfEscape(_) => true,
            Event::Macro(_) => false,
        }
    }

    /// Renders the event as wire-MIDI bytes for real-time output.
    ///
    /// File-only meta-events, SMF Escape events, and macros have no wire
    /// representation and are rejected.
    pub fn to_midi_bytes(&self) -> Result<Vec<u8>, EventError> {
        match self {
            Event::Channel(m) => m.encode(),
            Event::SysEx(data) => Ok(frame_sysex(data)),
            Event::Wire(bytes) => Ok(bytes.clone()),
            Event::Meta(_) | Event::SmfEscape(_) => {
                Err(EventError::Unsupported("a live MIDI message"))
            }
            Event::Macro(_) => Err(EventError::Unsupported("a live MIDI message")),
            Event::Empty => Err(EventError::Incomplete),
        }
    }

    /// Encodes the event as an SMF event body (without any delta-time).
    pub fn to_smf_bytes(&self) -> Result<Vec<u8>, EventError> {
        match self {
            Event::Channel(m) => m.encode(),
            Event::Meta(meta) => Ok(meta.encode()),
            Event::SysEx(data) => {
                let mut out = vec![0xF0];
                write_vlq(data.len() as u32, &mut out);
                out.extend_from_slice(data);
                Ok(out)
            }
            Event::SmfEscape(data) => {
                let mut out = vec![0xF7];
                write_vlq(data.len() as u32, &mut out);
                out.extend_from_slice(data);
                Ok(out)
            }
            Event::Wire(bytes) => wire_to_smf(bytes),
            Event::Macro(_) => Err(EventError::Unsupported("a Standard MIDI File event")),
            Event::Empty => Err(EventError::Incomplete),
        }
    }

    /// Encodes the event in the byte form accepted by `:event`.
    ///
    /// Channel, sysex, and generic wire events use live wire-MIDI bytes; meta
    /// and SMF Escape events use their standard SMF encoding; macros use their
    /// three inline bytes.
    pub fn to_event_bytes(&self) -> Result<Vec<u8>, EventError> {
        match self {
            Event::Empty => Err(EventError::Incomplete),
            Event::Channel(m) => m.encode(),
            Event::SysEx(data) => Ok(frame_sysex(data)),
            Event::Meta(meta) => Ok(meta.encode()),
            Event::SmfEscape(data) => {
                let mut out = vec![0xF7];
                write_vlq(data.len() as u32, &mut out);
                out.extend_from_slice(data);
                Ok(out)
            }
            Event::Macro(call) => Ok(call.bytes().to_vec()),
            Event::Wire(bytes) => Ok(bytes.clone()),
        }
    }

    /// Parses a `:event` byte sequence into a typed event.
    ///
    /// Exactly three bytes all below `0x80` are a macro call. Other input is
    /// interpreted as an SMF meta/escape encoding or as live wire-MIDI bytes;
    /// malformed input is rejected rather than stored.
    pub fn from_event_bytes(bytes: &[u8]) -> Result<Event, EventError> {
        if bytes.is_empty() {
            return Err(EventError::Incomplete);
        }
        if bytes.len() == 3 && bytes.iter().all(|b| *b < 0x80) {
            return Ok(Event::Macro(MacroCall::new(bytes[0], bytes[1], bytes[2])));
        }
        if bytes[0] == 0xFF {
            if let Some(event) = parse_meta_bytes(bytes)? {
                return Ok(event);
            }
        }
        if bytes[0] == 0xF7 {
            if let Some(event) = parse_escape_bytes(bytes)? {
                return Ok(event);
            }
        }
        if bytes[0] == 0xF0 {
            if bytes.last() != Some(&0xF7) || bytes.len() < 2 {
                return Err(EventError::InvalidWire(
                    "sysex must be framed by F0 and F7".to_string(),
                ));
            }
            let payload = &bytes[1..bytes.len() - 1];
            if payload.iter().any(|b| *b >= 0x80) {
                return Err(EventError::InvalidWire(
                    "sysex data byte out of range".to_string(),
                ));
            }
            return Ok(Event::SysEx(payload.to_vec()));
        }
        if (0x80..=0xEF).contains(&bytes[0]) {
            let kind = ChannelKind::from_status_nibble(bytes[0] >> 4)
                .ok_or(EventError::InvalidStatus(bytes[0]))?;
            let need = kind.data_len();
            if bytes.len() != 1 + need {
                return Err(EventError::InvalidWire(format!(
                    "{} needs {} data byte(s)",
                    kind.name(),
                    need
                )));
            }
            if bytes[1..].iter().any(|b| *b >= 0x80) {
                return Err(EventError::InvalidWire(
                    "data byte out of range".to_string(),
                ));
            }
            return Ok(Event::Channel(ChannelMessage::decode(
                bytes[0],
                &bytes[1..],
            )?));
        }
        if is_valid_wire(bytes) {
            return Ok(Event::Wire(bytes.to_vec()));
        }
        Err(EventError::InvalidWire(
            "unrecognized MIDI byte sequence".to_string(),
        ))
    }

    /// Decodes one SMF event body, returning the event and the bytes consumed.
    ///
    /// Running status is not handled here; the file layer resolves it first.
    pub fn from_smf_bytes(input: &[u8]) -> Result<(Event, usize), EventError> {
        let Some(&status) = input.first() else {
            return Err(EventError::UnexpectedEof);
        };
        match status {
            0xFF => {
                if input.len() < 2 {
                    return Err(EventError::UnexpectedEof);
                }
                let type_byte = input[1];
                let (len, used) = read_vlq(&input[2..]).map_err(|_| EventError::InvalidVlq)?;
                let start = 2 + used;
                let end = start + len as usize;
                if input.len() < end {
                    return Err(EventError::UnexpectedEof);
                }
                let meta = MetaEvent::from_parts(type_byte, input[start..end].to_vec());
                Ok((Event::Meta(meta), end))
            }
            0xF0 => {
                let (len, used) = read_vlq(&input[1..]).map_err(|_| EventError::InvalidVlq)?;
                let start = 1 + used;
                let end = start + len as usize;
                if input.len() < end {
                    return Err(EventError::UnexpectedEof);
                }
                Ok((Event::SysEx(input[start..end].to_vec()), end))
            }
            0xF7 => {
                let (len, used) = read_vlq(&input[1..]).map_err(|_| EventError::InvalidVlq)?;
                let start = 1 + used;
                let end = start + len as usize;
                if input.len() < end {
                    return Err(EventError::UnexpectedEof);
                }
                Ok((Event::SmfEscape(input[start..end].to_vec()), end))
            }
            0x80..=0xEF => {
                let kind = ChannelKind::from_status_nibble(status >> 4)
                    .ok_or(EventError::InvalidStatus(status))?;
                let need = kind.data_len();
                if input.len() < 1 + need {
                    return Err(EventError::UnexpectedEof);
                }
                let message = ChannelMessage::decode(status, &input[1..])?;
                Ok((Event::Channel(message), 1 + need))
            }
            other => Err(EventError::InvalidStatus(other)),
        }
    }

    /// Renders the event as inline hexadecimal, or `None` for complex events.
    pub fn inline_hex(&self) -> Option<String> {
        match self {
            Event::Channel(m) => Some(m.inline_hex()),
            Event::Macro(m) => Some(format!("{:02X}{:02X}{:02X}", m.id, m.arg1, m.arg2)),
            _ => None,
        }
    }
}

/// Frames a sysex payload with the wire `F0`/`F7` markers.
fn frame_sysex(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 2);
    out.push(0xF0);
    out.extend_from_slice(payload);
    out.push(0xF7);
    out
}

/// Converts validated wire bytes into a faithful SMF event body.
///
/// Channel messages are already file-representable; a wire sysex becomes an
/// SMF `F0` event. Every other generic wire form (system common, real-time,
/// and undefined messages) has no faithful SMF representation and is rejected.
fn wire_to_smf(bytes: &[u8]) -> Result<Vec<u8>, EventError> {
    match bytes.first() {
        Some(status) if (0x80..=0xEF).contains(status) => {
            let kind = ChannelKind::from_status_nibble(status >> 4)
                .ok_or(EventError::InvalidStatus(*status))?;
            if bytes.len() == 1 + kind.data_len() && bytes[1..].iter().all(|b| *b < 0x80) {
                Ok(bytes.to_vec())
            } else {
                Err(EventError::Unsupported("a Standard MIDI File event"))
            }
        }
        Some(0xF0) if bytes.len() >= 2 && bytes[bytes.len() - 1] == 0xF7 => {
            let payload = &bytes[1..bytes.len() - 1];
            let mut out = vec![0xF0];
            write_vlq(payload.len() as u32, &mut out);
            out.extend_from_slice(payload);
            Ok(out)
        }
        _ => Err(EventError::Unsupported("a Standard MIDI File event")),
    }
}

/// Whether `bytes` is exactly one valid live wire system message.
fn is_valid_wire(bytes: &[u8]) -> bool {
    match bytes {
        [0xF1, d] | [0xF3, d] => *d < 0x80,
        [0xF2, a, b] => *a < 0x80 && *b < 0x80,
        [0xF6] | [0xF7] | [0xF8] | [0xFA] | [0xFB] | [0xFC] | [0xFE] | [0xFF] => true,
        _ => false,
    }
}

/// Parses an exact SMF meta-event encoding (`FF type vlq data`).
fn parse_meta_bytes(bytes: &[u8]) -> Result<Option<Event>, EventError> {
    if bytes.len() < 2 {
        return Ok(None);
    }
    let type_byte = bytes[1];
    let (len, used) = match read_vlq(&bytes[2..]) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let start = 2 + used;
    let end = start + len as usize;
    if end != bytes.len() {
        return Ok(None);
    }
    Ok(Some(Event::Meta(MetaEvent::from_parts(
        type_byte,
        bytes[start..end].to_vec(),
    ))))
}

/// Parses an exact SMF Escape encoding (`F7 vlq data`).
fn parse_escape_bytes(bytes: &[u8]) -> Result<Option<Event>, EventError> {
    if bytes.len() < 2 {
        return Ok(None);
    }
    let (len, used) = match read_vlq(&bytes[1..]) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let start = 1 + used;
    let end = start + len as usize;
    if end != bytes.len() {
        return Ok(None);
    }
    Ok(Some(Event::SmfEscape(bytes[start..end].to_vec())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_flags_distinguish_omitted_from_explicit_zero() {
        let omitted = ChannelMessage {
            kind: ChannelKind::NoteOn,
            channel: Some(0),
            data1: Some(60),
            data2: None,
        };
        let explicit_zero = ChannelMessage {
            kind: ChannelKind::NoteOn,
            channel: Some(0),
            data1: Some(60),
            data2: Some(0),
        };
        assert_ne!(omitted, explicit_zero);
        assert!(!omitted.is_complete());
        assert_eq!(omitted.encode(), Err(EventError::Incomplete));
        assert_eq!(explicit_zero.encode().unwrap(), vec![0x90, 60, 0x00]);
    }

    #[test]
    fn incomplete_events_are_rejected() {
        let no_channel = ChannelMessage {
            kind: ChannelKind::NoteOn,
            channel: None,
            data1: Some(0),
            data2: Some(0),
        };
        assert_eq!(no_channel.encode(), Err(EventError::Incomplete));
        assert_eq!(Event::Empty.to_midi_bytes(), Err(EventError::Incomplete));
        let bad_channel = ChannelMessage {
            kind: ChannelKind::NoteOn,
            channel: Some(16),
            data1: Some(0),
            data2: Some(0),
        };
        assert_eq!(bad_channel.encode(), Err(EventError::ChannelOutOfRange(16)));
    }

    #[test]
    fn channel_messages_round_trip_all_kinds() {
        let cases = [
            ChannelMessage::with_data(ChannelKind::NoteOff, 1, 64, Some(0)),
            ChannelMessage::with_data(ChannelKind::NoteOn, 0, 127, Some(100)),
            ChannelMessage::with_data(ChannelKind::PolyAftertouch, 15, 60, Some(42)),
            ChannelMessage::with_data(ChannelKind::ControlChange, 3, 7, Some(127)),
            ChannelMessage::with_data(ChannelKind::ProgramChange, 9, 12, None),
            ChannelMessage::with_data(ChannelKind::ChannelAftertouch, 4, 88, None),
            ChannelMessage::with_data(ChannelKind::PitchBend, 2, 0, Some(64)),
        ];
        for original in cases {
            let bytes = original.encode().unwrap();
            let (decoded, used) = Event::from_smf_bytes(&bytes).unwrap();
            assert_eq!(used, bytes.len());
            assert_eq!(decoded, Event::Channel(original));
        }
    }

    #[test]
    fn meta_events_round_trip_by_type() {
        let cases = vec![
            MetaEvent::SequenceNumber(7),
            MetaEvent::text(TextKind::TrackName, b"Lead".to_vec()),
            MetaEvent::ChannelPrefix(3),
            MetaEvent::EndOfTrack,
            MetaEvent::Tempo(500_000),
            MetaEvent::SmpteOffset(SmpteOffset([1, 2, 3, 4, 5])),
            MetaEvent::TimeSignature(TimeSignature {
                numerator: 6,
                denominator_pow2: 3,
                clocks_per_click: 24,
                notated_32nds_per_quarter: 8,
            }),
            MetaEvent::KeySignature(KeySignature {
                sharps_flats: -3,
                is_minor: true,
            }),
            MetaEvent::SequencerSpecific(vec![0xDE, 0xAD, 0xBE, 0xEF]),
            MetaEvent::Unknown {
                kind: 0x60,
                data: vec![1, 0, 2, 0],
            },
        ];
        for meta in cases {
            let bytes = meta.encode();
            let (decoded, used) = Event::from_smf_bytes(&bytes).unwrap();
            assert_eq!(used, bytes.len());
            assert_eq!(decoded, Event::Meta(meta.clone()), "meta {meta:?}");
        }
    }

    #[test]
    fn raw_meta_payloads_are_preserved_when_typed_length_mismatches() {
        // SetTempo with a wrong length must not be coerced; it is kept verbatim.
        let bytes = [0xFF, 0x51, 0x02, 0x01, 0x02];
        let (decoded, used) = Event::from_smf_bytes(&bytes).unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(
            decoded,
            Event::Meta(MetaEvent::Unknown {
                kind: 0x51,
                data: vec![1, 2]
            })
        );
        let reencoded = decoded.to_smf_bytes().unwrap();
        assert_eq!(reencoded, bytes);
    }

    #[test]
    fn sysex_round_trips_and_preserves_payload() {
        let payload = vec![0x7D, 0x00, 0x11, 0x22];
        let event = Event::SysEx(payload);
        let smf = event.to_smf_bytes().unwrap();
        let (decoded, used) = Event::from_smf_bytes(&smf).unwrap();
        assert_eq!(used, smf.len());
        assert_eq!(decoded, event);
        // Wire form adds the F7 terminator.
        assert_eq!(
            event.to_midi_bytes().unwrap(),
            vec![0xF0, 0x7D, 0x00, 0x11, 0x22, 0xF7]
        );
    }

    #[test]
    fn macro_calls_expose_exactly_two_arguments() {
        let call = MacroCall::new(macro_id::BUILTIN_TRIGGER, 0x05, 0x00);
        assert_eq!(call.args(), [0x05, 0x00]);
        assert_eq!(call.bytes(), [0x01, 0x05, 0x00]);
        assert_eq!(Event::Macro(call).inline_hex().as_deref(), Some("010500"));
    }

    #[test]
    fn macro_id_ranges_match_the_spec() {
        assert!(macro_id::is_reserved(macro_id::SETUP));
        assert!(macro_id::is_builtin(0x01));
        assert!(macro_id::is_defined_builtin(0x01));
        assert!(macro_id::is_defined_builtin(0x02));
        assert!(!macro_id::is_defined_builtin(0x03));
        assert!(macro_id::is_user(0x40));
        assert!(macro_id::is_user(0x7F));
        assert!(!macro_id::is_user(0x80));
        assert!(macro_id::is_reserved(0x80));
        assert!(macro_id::is_reserved(0xFF));
    }

    #[test]
    fn inline_hex_uses_dots_for_absent_nibbles() {
        let partial = ChannelMessage {
            kind: ChannelKind::NoteOn,
            channel: Some(1),
            data1: None,
            data2: Some(0),
        };
        assert_eq!(partial.inline_hex(), "91··00");
        let empty_prefix = ChannelMessage::new(ChannelKind::ControlChange);
        assert_eq!(empty_prefix.inline_hex(), "··".to_owned() + "····");
        let program = ChannelMessage::with_data(ChannelKind::ProgramChange, 0, 5, None);
        assert_eq!(program.inline_hex(), "C005");
    }

    #[test]
    fn event_classification() {
        assert_eq!(Event::Empty.class(), EventClass::Empty);
        assert_eq!(
            Event::Channel(ChannelMessage::new(ChannelKind::NoteOn)).class(),
            EventClass::Inline
        );
        assert_eq!(
            Event::Macro(MacroCall::new(1, 0, 0)).class(),
            EventClass::Inline
        );
        assert_eq!(
            Event::Meta(MetaEvent::EndOfTrack).class(),
            EventClass::Complex
        );
        assert_eq!(Event::SysEx(vec![1]).class(), EventClass::Complex);
        assert_eq!(Event::Wire(vec![0xAB]).class(), EventClass::Complex);
    }

    #[test]
    fn decode_rejects_bad_status_and_truncation() {
        assert_eq!(
            Event::from_smf_bytes(&[0x7F]),
            Err(EventError::InvalidStatus(0x7F))
        );
        assert_eq!(
            Event::from_smf_bytes(&[0x90, 0x40]),
            Err(EventError::UnexpectedEof)
        );
        assert_eq!(Event::from_smf_bytes(&[]), Err(EventError::UnexpectedEof));
    }

    #[test]
    fn event_bytes_parse_and_round_trip_every_form() {
        let cases = vec![
            Event::Channel(ChannelMessage::with_data(
                ChannelKind::NoteOn,
                3,
                60,
                Some(100),
            )),
            Event::Channel(ChannelMessage::with_data(
                ChannelKind::ProgramChange,
                0,
                12,
                None,
            )),
            Event::SysEx(vec![0x7D, 0x01, 0x02]),
            Event::Meta(MetaEvent::Tempo(400_000)),
            Event::Meta(MetaEvent::text(TextKind::Marker, b"hi".to_vec())),
            Event::SmfEscape(vec![0x01, 0x02, 0x03]),
            Event::Macro(MacroCall::new(0x41, 5, 6)),
            Event::Wire(vec![0xF8]),
            Event::Wire(vec![0xF2, 0x01, 0x02]),
        ];
        for event in cases {
            let bytes = event.to_event_bytes().expect("encodable");
            let parsed = Event::from_event_bytes(&bytes)
                .unwrap_or_else(|e| panic!("failed to parse {bytes:02X?} from {event:?}: {e}"));
            assert_eq!(parsed, event, "round-trip for {event:?}");
        }
    }

    #[test]
    fn exactly_three_low_bytes_become_a_macro() {
        assert_eq!(
            Event::from_event_bytes(&[0x01, 0x05, 0x00]).unwrap(),
            Event::Macro(MacroCall::new(1, 5, 0))
        );
        // A channel message with the same first nibble is not a macro.
        assert!(matches!(
            Event::from_event_bytes(&[0x90, 0x40, 0x7F]),
            Ok(Event::Channel(_))
        ));
    }

    #[test]
    fn malformed_event_bytes_are_rejected() {
        // Odd/incomplete channel message.
        assert!(Event::from_event_bytes(&[0x90, 0x40]).is_err());
        // Data byte out of range.
        assert!(Event::from_event_bytes(&[0x90, 0x40, 0xFF]).is_err());
        // Unframed sysex.
        assert!(Event::from_event_bytes(&[0xF0, 0x01, 0x02]).is_err());
        // Undefined system message.
        assert!(Event::from_event_bytes(&[0xF4]).is_err());
        // Empty.
        assert!(Event::from_event_bytes(&[]).is_err());
    }

    #[test]
    fn wire_to_smf_only_succeeds_for_faithful_forms() {
        // Channel and sysex wire forms have a faithful file representation.
        assert_eq!(
            Event::Wire(vec![0x90, 0x40, 0x7F]).to_smf_bytes().unwrap(),
            vec![0x90, 0x40, 0x7F]
        );
        assert_eq!(
            Event::Wire(vec![0xF0, 0x7D, 0x01, 0xF7])
                .to_smf_bytes()
                .unwrap(),
            vec![0xF0, 0x02, 0x7D, 0x01]
        );
        // Real-time/system-common wire forms do not.
        assert!(Event::Wire(vec![0xF8]).to_smf_bytes().is_err());
        assert!(Event::Wire(vec![0xF2, 0x01, 0x02]).to_smf_bytes().is_err());
        // SMF Escape has no wire representation.
        assert!(Event::SmfEscape(vec![1]).to_midi_bytes().is_err());
    }

    #[test]
    fn smf_escape_round_trips_through_smf_bytes() {
        let event = Event::SmfEscape(vec![0xDE, 0xAD]);
        let bytes = event.to_smf_bytes().unwrap();
        assert_eq!(bytes, vec![0xF7, 0x02, 0xDE, 0xAD]);
        let (decoded, used) = Event::from_smf_bytes(&bytes).unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(decoded, event);
    }
}
