//! The copy/paste clipboard for nibble-level block editing.
//!
//! The clipboard stores a rectangular matrix of nibble values plus whether
//! offset nibbles are present. `None` entries mean "leave the destination
//! nibble unchanged", which keeps paste alignment predictable when the source
//! event is shorter than the destination cell.

use mtrak_core::composition::Pattern;

use crate::edit::{event_nibbles, CellEdit, EVENT_NIBBLES, OFFSET_NIBBLES};

/// A rectangular block of nibbles copied out of a pattern.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NibbleClipboard {
    /// Rows of nibble values (`None` = blank/skip).
    pub cells: Vec<Vec<Option<u8>>>,
    /// Whether offset nibbles were included in the copy.
    pub offsets_included: bool,
    /// Number of nibbles per cell at copy time.
    pub nibbles_per_cell: usize,
}

impl NibbleClipboard {
    /// Whether the clipboard holds nothing.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The number of rows.
    pub fn height(&self) -> usize {
        self.cells.len()
    }

    /// The number of nibble columns.
    pub fn width(&self) -> usize {
        self.cells.first().map(Vec::len).unwrap_or(0)
    }

    /// Copies a rectangular nibble range out of `pattern`.
    ///
    /// `top..=bottom` are rows and `left..=right` are absolute nibble columns.
    pub fn from_pattern(
        pattern: &Pattern,
        top: usize,
        bottom: usize,
        left: usize,
        right: usize,
        offsets_included: bool,
    ) -> Self {
        let nibbles_per_cell = if offsets_included {
            EVENT_NIBBLES + OFFSET_NIBBLES
        } else {
            EVENT_NIBBLES
        };
        let mut cells = Vec::new();
        for row in top..=bottom {
            let mut line = Vec::new();
            for column in left..=right {
                let track = column / nibbles_per_cell;
                let nibble = column % nibbles_per_cell;
                line.push(read_nibble(pattern, row, track, nibble, nibbles_per_cell));
            }
            cells.push(line);
        }
        NibbleClipboard {
            cells,
            offsets_included,
            nibbles_per_cell,
        }
    }
}

fn read_nibble(
    pattern: &Pattern,
    row: usize,
    track: usize,
    nibble: usize,
    nibbles_per_cell: usize,
) -> Option<u8> {
    let slot = pattern.slot(row, track)?;
    if nibble < EVENT_NIBBLES {
        return event_nibbles(&slot.event)[nibble];
    }
    if nibbles_per_cell > EVENT_NIBBLES {
        let offset_index = nibble - EVENT_NIBBLES;
        let shift = (OFFSET_NIBBLES - 1 - offset_index) * 4;
        return Some(((slot.offset >> shift) & 0x0F) as u8);
    }
    None
}

/// Loads a fresh edit buffer for the destination cell, reusing the slot content
/// when it holds an inline event.
pub fn destination_edit(
    pattern: &Pattern,
    row: usize,
    track: usize,
    offsets_included: bool,
) -> CellEdit {
    match pattern.slot(row, track) {
        Some(slot) => CellEdit::from_slot(row, track, slot, offsets_included),
        None => CellEdit::blank(row, track, offsets_included),
    }
}
