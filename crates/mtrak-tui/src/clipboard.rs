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
    /// Rows of nibble values (`None` = blank/zero content).
    pub cells: Vec<Vec<Option<u8>>>,
    /// Whether offset nibbles were included in the copy.
    pub offsets_included: bool,
    /// Number of nibbles per cell at copy time.
    pub nibbles_per_cell: usize,
    /// The source nibble index of the first copied column.
    pub origin_nib: usize,
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
            origin_nib: left % nibbles_per_cell,
        }
    }

    /// Maps the clipboard to absolute destination columns.
    ///
    /// Each entry is `(row_offset, destination_column, value)`. When the source
    /// and destination cell widths match the mapping is a plain linear offset
    /// from `origin_col`, preserving the exact cursor origin. When offset
    /// visibility differs it maps event nibbles to event columns and offset
    /// nibbles to offset columns (skipping offsets when the destination hides
    /// them) so event data never spills into offset columns. In-rectangle
    /// blanks are emitted as `0` because they are meaningful zero content.
    pub fn mapped_columns(
        &self,
        origin_col: usize,
        dst_npc: usize,
        dst_offsets: bool,
    ) -> Vec<(usize, usize, u8)> {
        let src_npc = self.nibbles_per_cell.max(1);
        let dst_npc = dst_npc.max(1);
        let origin_cell = origin_col / dst_npc;
        let mut out = Vec::new();
        for (dr, line) in self.cells.iter().enumerate() {
            for (dc, value) in line.iter().enumerate() {
                let dest_col = if src_npc == dst_npc {
                    origin_col + dc
                } else {
                    let total = self.origin_nib + dc;
                    let relative_cell = total / src_npc;
                    let source_nib = total % src_npc;
                    let dest_nib = if source_nib < EVENT_NIBBLES {
                        source_nib
                    } else if dst_offsets {
                        EVENT_NIBBLES + (source_nib - EVENT_NIBBLES)
                    } else {
                        continue;
                    };
                    (origin_cell + relative_cell) * dst_npc + dest_nib
                };
                out.push((dr, dest_col, value.unwrap_or(0)));
            }
        }
        out
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

#[cfg(test)]
mod tests {
    use super::*;
    use mtrak_core::composition::{Pattern, PatternId};
    use mtrak_core::event::{ChannelKind, ChannelMessage, Event};

    fn note() -> Event {
        Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            0,
            60,
            Some(100),
        ))
    }

    #[test]
    fn mapped_columns_preserve_the_exact_origin() {
        let mut pattern = Pattern::new(PatternId(0), 4, 4, 2);
        pattern.set_event(0, 0, note());
        let clip = NibbleClipboard::from_pattern(&pattern, 0, 0, 0, 5, false);
        let mapped = clip.mapped_columns(3, 6, false);
        assert_eq!(mapped[0], (0, 3, 0x9));
        assert_eq!(mapped[1], (0, 4, 0x0));
        assert_eq!(mapped[5], (0, 8, 0x4), "columns advance linearly");
    }

    #[test]
    fn mapped_columns_skip_offsets_when_the_destination_hides_them() {
        let mut pattern = Pattern::new(PatternId(0), 4, 4, 1);
        pattern.set_event(0, 0, note());
        pattern.slot_mut(0, 0).unwrap().offset = 3;
        let clip = NibbleClipboard::from_pattern(&pattern, 0, 0, 0, 9, true);
        let mapped = clip.mapped_columns(0, 6, false);
        assert_eq!(
            mapped.iter().map(|(_, col, _)| *col).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5],
            "offset nibbles are clipped, not spilled"
        );
    }

    #[test]
    fn mapped_columns_remap_cells_when_offsets_become_visible() {
        let mut pattern = Pattern::new(PatternId(0), 4, 4, 1);
        pattern.set_event(0, 0, note());
        let clip = NibbleClipboard::from_pattern(&pattern, 0, 0, 0, 5, false);
        // One event cell copied without offsets.
        let mapped = clip.mapped_columns(0, 10, true);
        assert_eq!(mapped.len(), 6);
        assert_eq!(mapped[0], (0, 0, 0x9));
        assert_eq!(
            mapped[5],
            (0, 5, 0x4),
            "event bytes stay in event columns and do not reach column 6"
        );
    }
}
