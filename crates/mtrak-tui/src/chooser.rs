//! The centered 16×16 pattern/module chooser.
//!
//! The chooser represents IDs `00`–`FF` and supports cursor-key navigation,
//! hexadecimal jumps, Enter to accept, and Esc to cancel. Rendering and the
//! accept/clipboard semantics live in [`crate::app`] because they depend on the
//! project.

/// Which namespace the chooser is selecting from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChooserKind {
    /// A pattern ID.
    Pattern,
    /// A module ID.
    Code,
}

/// Chooser navigation state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chooser {
    /// Which namespace is being chosen.
    pub kind: ChooserKind,
    /// The highlighted ID.
    pub cursor: u8,
    /// Hexadecimal digits of the byte being typed (0–2).
    ///
    /// The digits are kept after a jump so the grid can keep showing which row
    /// and column they selected, until the next digit or cursor movement.
    pub hex: String,
}

impl Chooser {
    /// Opens a chooser of `kind` at `cursor`.
    pub fn new(kind: ChooserKind, cursor: u8) -> Self {
        Chooser {
            kind,
            cursor,
            hex: String::new(),
        }
    }

    /// Moves the cursor by one cell, clamped.
    pub fn move_by(&mut self, dx: i32, dy: i32) {
        // Moving abandons a partially typed byte, clearing its guide lines.
        self.hex.clear();
        let row = (self.cursor / 16) as i32;
        let col = (self.cursor % 16) as i32;
        let row = (row + dy).clamp(0, 15);
        let col = (col + dx).clamp(0, 15);
        self.cursor = (row * 16 + col) as u8;
    }

    /// Feeds a hexadecimal digit, jumping when two have been entered.
    pub fn type_hex(&mut self, digit: char) {
        let Some(value) = digit.to_digit(16) else {
            return;
        };
        // A fresh byte replaces the previous one after a jump.
        if self.hex.len() >= 2 {
            self.hex.clear();
        }
        self.hex.push(
            char::from_digit(value, 16)
                .unwrap_or('0')
                .to_ascii_uppercase(),
        );
        if self.hex.len() == 2 {
            if let Ok(id) = u8::from_str_radix(&self.hex, 16) {
                self.cursor = id;
            }
        }
    }

    /// The row, and column once two digits are typed, selected by `hex`.
    ///
    /// The first digit is the high nibble, so it fixes the row;
    /// the second one fixes the column.
    pub fn typed_row_col(&self) -> Option<(u8, Option<u8>)> {
        let mut digits = self.hex.chars().filter_map(|c| c.to_digit(16));
        let row = digits.next()? as u8;
        Some((row, digits.next().map(|col| col as u8)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_wraps_within_the_16x16_grid() {
        let mut chooser = Chooser::new(ChooserKind::Pattern, 0);
        chooser.move_by(-1, 0);
        assert_eq!(chooser.cursor, 0);
        chooser.move_by(0, -1);
        assert_eq!(chooser.cursor, 0);
        chooser.move_by(16, 0);
        assert_eq!(chooser.cursor, 15);
        chooser.move_by(0, 1);
        assert_eq!(chooser.cursor, 31);
        chooser.move_by(0, 20);
        assert_eq!(chooser.cursor, 0xFF);
    }

    #[test]
    fn two_hex_digits_jump_to_a_cell() {
        let mut chooser = Chooser::new(ChooserKind::Code, 0);
        chooser.type_hex('4');
        assert_eq!(chooser.cursor, 0, "no jump until two digits");
        chooser.type_hex('f');
        assert_eq!(chooser.cursor, 0x4F);
        assert_eq!(chooser.hex, "4F", "the typed byte stays visible");
    }

    #[test]
    fn the_typed_prefix_selects_a_row_and_then_a_column() {
        let mut chooser = Chooser::new(ChooserKind::Pattern, 0);
        assert_eq!(chooser.typed_row_col(), None);
        chooser.type_hex('4');
        assert_eq!(chooser.typed_row_col(), Some((4, None)));
        chooser.type_hex('f');
        assert_eq!(chooser.typed_row_col(), Some((4, Some(15))));
    }

    #[test]
    fn a_third_digit_starts_a_new_byte() {
        let mut chooser = Chooser::new(ChooserKind::Pattern, 0);
        for digit in "4f1".chars() {
            chooser.type_hex(digit);
        }
        assert_eq!(chooser.hex, "1");
        assert_eq!(chooser.typed_row_col(), Some((1, None)));
        assert_eq!(chooser.cursor, 0x4F, "the jump stays until the next byte");
    }

    #[test]
    fn moving_discards_the_typed_prefix() {
        let mut chooser = Chooser::new(ChooserKind::Pattern, 0);
        chooser.type_hex('4');
        chooser.type_hex('f');
        chooser.move_by(0, -1);
        assert!(chooser.hex.is_empty());
        assert_eq!(chooser.typed_row_col(), None);
        assert_eq!(chooser.cursor, 0x3F);
    }
}
