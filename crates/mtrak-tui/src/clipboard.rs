//! The copy/paste clipboard, storing rectangular blocks of slots.

use mtrak_core::composition::{Pattern, Slot};

use crate::app::Rect;

/// A rectangular block of copied slots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Clipboard {
    /// Rows of slots.
    pub cells: Vec<Vec<Slot>>,
}

impl Clipboard {
    /// Copies the rectangle out of a pattern.
    pub fn from_pattern(pattern: &Pattern, rect: Rect) -> Self {
        let mut cells = Vec::new();
        for row in rect.top..=rect.bottom {
            let mut line = Vec::new();
            for track in rect.left..=rect.right {
                line.push(pattern.slot(row, track).cloned().unwrap_or_default());
            }
            cells.push(line);
        }
        Clipboard { cells }
    }

    /// Whether the clipboard holds nothing.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The number of rows.
    pub fn height(&self) -> usize {
        self.cells.len()
    }

    /// The number of tracks.
    pub fn width(&self) -> usize {
        self.cells.first().map(Vec::len).unwrap_or(0)
    }
}
