//! Temporary nibble edit buffers for the pattern grid.
//!
//! A cell being edited holds a six-nibble event buffer and, when offsets are
//! visible, a four-nibble offset buffer. The buffers are the source of truth
//! while they contain blanks or an intermediate value that cannot yet be
//! represented as a valid event; conversion to a typed event is attempted after
//! every keystroke, and blanks are zero-filled when the user leaves the cell.

use mtrak_core::composition::Slot;
use mtrak_core::event::{ChannelKind, ChannelMessage, Event, MacroCall};

/// Number of event nibbles in one inline cell.
pub const EVENT_NIBBLES: usize = 6;
/// Number of offset nibbles when offsets are visible.
pub const OFFSET_NIBBLES: usize = 4;

/// A temporary edit buffer for one pattern cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellEdit {
    /// The row being edited.
    pub row: usize,
    /// The track being edited.
    pub track: usize,
    /// Event nibbles (`None` = blank).
    pub event: [Option<u8>; EVENT_NIBBLES],
    /// Offset nibbles (`None` = blank).
    pub offset: [Option<u8>; OFFSET_NIBBLES],
    /// Whether the offset buffer participates in the cursor.
    pub offset_visible: bool,
    /// Whether the current offset value is out of range (displayed red).
    pub offset_invalid: bool,
}

impl CellEdit {
    /// A blank buffer for an empty cell.
    pub fn blank(row: usize, track: usize, offset_visible: bool) -> Self {
        CellEdit {
            row,
            track,
            event: [None; EVENT_NIBBLES],
            offset: [None; OFFSET_NIBBLES],
            offset_visible,
            offset_invalid: false,
        }
    }

    /// A buffer populated from a slot, or blank for a complex/empty event.
    pub fn from_slot(row: usize, track: usize, slot: &Slot, offset_visible: bool) -> Self {
        let mut edit = CellEdit::blank(row, track, offset_visible);
        edit.event = event_nibbles(&slot.event);
        let offset = slot.offset;
        for (i, nibble) in edit.offset.iter_mut().enumerate() {
            let shift = (OFFSET_NIBBLES - 1 - i) * 4;
            *nibble = Some(((offset >> shift) & 0x0F) as u8);
        }
        edit
    }

    /// The number of addressable nibbles in the cell.
    pub fn nibble_count(&self) -> usize {
        if self.offset_visible {
            EVENT_NIBBLES + OFFSET_NIBBLES
        } else {
            EVENT_NIBBLES
        }
    }

    /// The value of event byte `index`, if both nibbles are present.
    pub fn event_byte(&self, index: usize) -> Option<u8> {
        let hi = self.event.get(index * 2).copied().flatten()?;
        let lo = self.event.get(index * 2 + 1).copied().flatten()?;
        Some((hi << 4) | lo)
    }

    /// Sets the nibble at `index` (event first, then offset when visible).
    pub fn set_nibble(&mut self, index: usize, value: u8) {
        let value = value & 0x0F;
        if index < EVENT_NIBBLES {
            self.event[index] = Some(value);
        } else if self.offset_visible && index < EVENT_NIBBLES + OFFSET_NIBBLES {
            self.offset[index - EVENT_NIBBLES] = Some(value);
            self.offset_invalid = false;
        }
    }

    /// Whether every event nibble is blank.
    pub fn event_is_blank(&self) -> bool {
        self.event.iter().all(Option::is_none)
    }

    /// Converts the buffer to a typed event when it is complete and valid.
    ///
    /// Incomplete input, reserved macro IDs, and non-inline status forms return
    /// `None`; the caller keeps the buffer rather than corrupting the slot.
    pub fn try_event(&self) -> Option<Event> {
        if self.event_is_blank() {
            return None;
        }
        let status = self.event_byte(0)?;
        if status < 0x80 {
            let arg1 = self.event_byte(1)?;
            let arg2 = self.event_byte(2)?;
            if status == 0 {
                return None;
            }
            return Some(Event::Macro(MacroCall::new(status, arg1, arg2)));
        }
        let kind = ChannelKind::from_status_nibble(status >> 4)?;
        let data1 = self.event_byte(1)?;
        if kind.data_len() == 1 {
            return Some(Event::Channel(ChannelMessage {
                kind,
                channel: Some(status & 0x0F),
                data1: Some(data1),
                data2: None,
            }));
        }
        let data2 = self.event_byte(2)?;
        Some(Event::Channel(ChannelMessage {
            kind,
            channel: Some(status & 0x0F),
            data1: Some(data1),
            data2: Some(data2),
        }))
    }

    /// The event committed when the user leaves the cell.
    ///
    /// Blanks are zero-filled; a normalized value that is not valid (for
    /// example reserved macro `00`) clears the slot instead.
    pub fn finalize_event(&self) -> Event {
        let mut filled = self.clone();
        for nibble in filled.event.iter_mut() {
            *nibble = Some(nibble.unwrap_or(0));
        }
        filled.try_event().unwrap_or(Event::Empty)
    }

    /// The offset value if every nibble is present and the value is in range.
    pub fn try_offset(&self, max: u32) -> Option<u32> {
        let mut value = 0u32;
        for nibble in self.offset {
            let nibble = nibble?;
            value = value * 16 + u32::from(nibble);
        }
        (value < max).then_some(value)
    }

    /// The offset committed when the user leaves the cell, clamped in range.
    pub fn finalize_offset(&self, max: u32) -> u32 {
        let mut value = 0u32;
        for nibble in self.offset {
            value = value * 16 + u32::from(nibble.unwrap_or(0));
        }
        value.min(max.saturating_sub(1))
    }

    /// Renders the event nibbles, using `·` for blanks.
    pub fn event_text(&self) -> String {
        self.event
            .iter()
            .map(|n| nibble_char(*n))
            .collect::<String>()
    }

    /// Renders the offset nibbles, using `·` for blanks.
    pub fn offset_text(&self) -> String {
        self.offset
            .iter()
            .map(|n| nibble_char(*n))
            .collect::<String>()
    }
}

/// Extracts the six inline nibbles from an event, blank for complex events.
pub fn event_nibbles(event: &Event) -> [Option<u8>; EVENT_NIBBLES] {
    let mut out = [None; EVENT_NIBBLES];
    let bytes: Vec<Option<u8>> = match event {
        Event::Channel(message) => {
            let status = message
                .channel
                .map(|channel| (message.kind.status_nibble() << 4) | (channel & 0x0F));
            let mut bytes = vec![status, message.data1];
            if message.kind.data_len() == 2 {
                bytes.push(message.data2);
            }
            bytes
        }
        Event::Macro(call) => vec![Some(call.id), Some(call.arg1), Some(call.arg2)],
        _ => return out,
    };
    for (index, byte) in bytes.iter().enumerate() {
        if let Some(byte) = byte {
            out[index * 2] = Some(byte >> 4);
            out[index * 2 + 1] = Some(byte & 0x0F);
        }
    }
    out
}

fn nibble_char(nibble: Option<u8>) -> char {
    match nibble {
        Some(value) => char::from_digit(u32::from(value & 0x0F), 16)
            .unwrap_or('?')
            .to_ascii_uppercase(),
        None => '·',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mtrak_core::event::MacroCall;

    fn note() -> Event {
        Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            0,
            60,
            Some(100),
        ))
    }

    #[test]
    fn existing_inline_events_populate_the_buffer() {
        let slot = Slot {
            event: note(),
            offset: 0x12,
        };
        let edit = CellEdit::from_slot(0, 0, &slot, true);
        assert_eq!(edit.event_text(), "903C64");
        assert_eq!(edit.offset_text(), "0012");
        assert_eq!(edit.try_event(), Some(note()));
        assert_eq!(edit.try_offset(24), Some(0x12));
    }

    #[test]
    fn one_data_byte_events_leave_the_last_nibbles_blank() {
        let program = Event::Channel(ChannelMessage::with_data(
            ChannelKind::ProgramChange,
            0,
            5,
            None,
        ));
        let edit = CellEdit::from_slot(
            0,
            0,
            &Slot {
                event: program.clone(),
                offset: 0,
            },
            false,
        );
        assert_eq!(edit.event_text(), "C005··");
        assert_eq!(edit.try_event(), Some(program));
    }

    #[test]
    fn empty_buffers_begin_blank_and_finalize_to_empty() {
        let mut edit = CellEdit::blank(0, 0, false);
        assert!(edit.event_is_blank());
        assert_eq!(edit.try_event(), None);
        assert_eq!(edit.finalize_event(), Event::Empty);
        // Typing a valid channel message commits it.
        for (i, nibble) in [9, 0, 3, 0xC, 6, 4].into_iter().enumerate() {
            edit.set_nibble(i, nibble);
        }
        assert_eq!(edit.try_event(), Some(note()));
    }

    #[test]
    fn reserved_macro_zero_is_normalized_away() {
        let mut edit = CellEdit::blank(0, 0, false);
        for i in 0..6 {
            edit.set_nibble(i, 0);
        }
        assert_eq!(edit.try_event(), None);
        assert_eq!(edit.finalize_event(), Event::Empty);
    }

    #[test]
    fn macros_are_exactly_three_low_bytes() {
        let mut edit = CellEdit::blank(0, 0, false);
        for (i, nibble) in [4, 1, 0, 5, 0, 6].into_iter().enumerate() {
            edit.set_nibble(i, nibble);
        }
        assert_eq!(
            edit.try_event(),
            Some(Event::Macro(MacroCall::new(0x41, 0x05, 0x06)))
        );
    }

    #[test]
    fn offset_validation_and_clamping() {
        let mut edit = CellEdit::blank(0, 0, true);
        for (i, nibble) in [0, 0, 0, 5].into_iter().enumerate() {
            edit.set_nibble(EVENT_NIBBLES + i, nibble);
        }
        assert_eq!(edit.try_offset(24), Some(5));
        // 0x0018 == 24, which is outside the valid `0..24` range.
        edit.set_nibble(EVENT_NIBBLES + 2, 0x1);
        edit.set_nibble(EVENT_NIBBLES + 3, 0x8);
        assert_eq!(edit.try_offset(24), None, "out of range stays buffered");
        assert_eq!(edit.finalize_offset(24), 23, "clamped to the last tick");
    }

    #[test]
    fn status_byte_switch_activates_preserved_nibbles() {
        // Start as program change, leaving the last two nibbles as data for a
        // later status change within the same session.
        let mut edit = CellEdit::blank(0, 0, false);
        for (i, nibble) in [0xC, 0, 0, 5, 0, 7].into_iter().enumerate() {
            edit.set_nibble(i, nibble);
        }
        assert_eq!(
            edit.try_event(),
            Some(Event::Channel(ChannelMessage::with_data(
                ChannelKind::ProgramChange,
                0,
                5,
                None
            )))
        );
        // Switch to a two-data-byte status; the preserved nibbles become data2.
        edit.set_nibble(0, 0x9);
        assert_eq!(
            edit.try_event(),
            Some(Event::Channel(ChannelMessage::with_data(
                ChannelKind::NoteOn,
                0,
                5,
                Some(7)
            )))
        );
    }
}
