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
    /// Hexadecimal digits typed so far (for direct jumps).
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
        let row = (self.cursor / 16) as i32;
        let col = (self.cursor % 16) as i32;
        let row = (row + dy).clamp(0, 15);
        let col = (col + dx).clamp(0, 15);
        self.cursor = (row * 16 + col) as u8;
    }

    /// Feeds a hexadecimal digit, jumping when two have been entered.
    pub fn type_hex(&mut self, digit: char) {
        if let Some(value) = digit.to_digit(16) {
            self.hex.push(
                char::from_digit(value, 16)
                    .unwrap_or('0')
                    .to_ascii_uppercase(),
            );
            if self.hex.len() == 2 {
                if let Ok(id) = u8::from_str_radix(&self.hex, 16) {
                    self.cursor = id;
                }
                self.hex.clear();
            }
        }
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
        assert!(chooser.hex.is_empty());
    }
}
