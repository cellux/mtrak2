//! The full event editor.
//!
//! Complex events are edited as their raw byte representation; committing the
//! buffer re-parses it into a typed event. The editor round-trips every event
//! type that has a byte representation, which is what keeps the model and the
//! display in agreement (P8 acceptance).

use mtrak_core::event::{Event, MacroCall};

/// How an editor buffer is interpreted when committed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditorKind {
    /// A Standard MIDI File event body (channel, sysex, or meta).
    Smf,
    /// A tracker macro call: `id arg1 arg2`.
    Macro,
    /// Unrecognized bytes preserved verbatim.
    Raw,
}

/// An editable event buffer with a nibble cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventEditor {
    /// The row being edited.
    pub row: usize,
    /// The track being edited.
    pub track: usize,
    /// How to interpret the buffer on commit.
    pub kind: EditorKind,
    /// The raw bytes being edited.
    pub bytes: Vec<u8>,
    /// The cursor position in nibbles from the start of the buffer.
    pub cursor: usize,
}

impl EventEditor {
    /// Opens an editor for `event`, or `None` for an empty slot.
    pub fn from_event(row: usize, track: usize, event: &Event) -> Option<EventEditor> {
        let (kind, bytes) = match event {
            Event::Empty => return None,
            Event::Macro(call) => (EditorKind::Macro, call.bytes().to_vec()),
            Event::Raw(bytes) => (EditorKind::Raw, bytes.clone()),
            other => (EditorKind::Smf, other.to_smf_bytes().ok()?),
        };
        Some(EventEditor {
            row,
            track,
            kind,
            bytes,
            cursor: 0,
        })
    }

    /// The number of nibbles in the buffer.
    pub fn nibble_count(&self) -> usize {
        self.bytes.len() * 2
    }

    /// Renders the buffer as hex, wrapping the nibble under the cursor in
    /// brackets.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, byte) in self.bytes.iter().enumerate() {
            let hi = i * 2;
            let lo = hi + 1;
            out.push_str(&nibble_text((byte >> 4) & 0x0F, self.cursor == hi));
            out.push_str(&nibble_text(byte & 0x0F, self.cursor == lo));
        }
        out
    }

    /// Moves the cursor left by one nibble.
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor right by one nibble, clamped to the buffer.
    pub fn move_right(&mut self) {
        if self.cursor + 1 < self.nibble_count().max(1) {
            self.cursor += 1;
        }
    }

    /// Sets the nibble under the cursor to `value` (`0..=15`).
    pub fn set_nibble(&mut self, value: u8) {
        let value = value & 0x0F;
        let byte_index = self.cursor / 2;
        let Some(byte) = self.bytes.get_mut(byte_index) else {
            return;
        };
        if self.cursor % 2 == 0 {
            *byte = (*byte & 0x0F) | (value << 4);
        } else {
            *byte = (*byte & 0xF0) | value;
        }
    }

    /// Commits the buffer back into a typed event.
    pub fn commit(&self) -> Event {
        match self.kind {
            EditorKind::Macro => {
                let id = self.bytes.first().copied().unwrap_or(0);
                let arg1 = self.bytes.get(1).copied().unwrap_or(0);
                let arg2 = self.bytes.get(2).copied().unwrap_or(0);
                Event::Macro(MacroCall::new(id, arg1, arg2))
            }
            EditorKind::Raw => Event::Raw(self.bytes.clone()),
            EditorKind::Smf => match Event::from_smf_bytes(&self.bytes) {
                Ok((event, _)) => event,
                Err(_) => Event::Raw(self.bytes.clone()),
            },
        }
    }
}

fn nibble_text(value: u8, selected: bool) -> String {
    let hex = char::from_digit(u32::from(value), 16)
        .unwrap_or('?')
        .to_ascii_uppercase();
    if selected {
        format!("[{hex}]")
    } else {
        hex.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mtrak_core::event::{
        ChannelKind, ChannelMessage, MetaEvent, SmpteOffset, TextKind, TimeSignature,
    };

    #[test]
    fn every_represented_event_round_trips_through_the_editor() {
        let events = vec![
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
            Event::Meta(MetaEvent::TimeSignature(TimeSignature {
                numerator: 6,
                denominator_pow2: 3,
                clocks_per_click: 24,
                notated_32nds_per_quarter: 8,
            })),
            Event::Meta(MetaEvent::SmpteOffset(SmpteOffset([1, 2, 3, 4, 5]))),
            Event::Meta(MetaEvent::Unknown {
                kind: 0x60,
                data: vec![9, 9],
            }),
            Event::Macro(MacroCall::new(0x41, 5, 6)),
            Event::Raw(vec![0xAB, 0xCD]),
        ];
        for event in events {
            let editor = EventEditor::from_event(0, 0, &event).expect("editable");
            assert_eq!(editor.commit(), event, "round-trip for {event:?}");
        }
        assert!(EventEditor::from_event(0, 0, &Event::Empty).is_none());
    }

    #[test]
    fn editing_a_nibble_changes_the_committed_event() {
        let original = Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            0,
            60,
            Some(100),
        ));
        let mut editor = EventEditor::from_event(0, 0, &original).unwrap();
        // Buffer is [status, data1, data2]; move to data1's high nibble and set to 7 (0x7C = 124).
        editor.move_right();
        editor.move_right();
        editor.set_nibble(0x7);
        let committed = editor.commit();
        assert_eq!(
            committed,
            Event::Channel(ChannelMessage::with_data(
                ChannelKind::NoteOn,
                0,
                0x7C,
                Some(100)
            ))
        );
    }
}
