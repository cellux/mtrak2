//! A single-line text buffer with a character cursor.
//!
//! Command mode uses this for standard line editing: Left/Right and Home/End
//! movement, insertion at the cursor, and Backspace/Delete at the cursor.

/// A single-line editor over `char`s.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineEditor {
    chars: Vec<char>,
    cursor: usize,
}

impl LineEditor {
    /// An empty editor.
    pub fn new() -> Self {
        LineEditor::default()
    }

    /// An editor over `text`, with the cursor at the start.
    pub fn from_text(text: &str) -> Self {
        LineEditor {
            chars: text.chars().collect(),
            cursor: 0,
        }
    }

    /// The current text.
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// The cursor position in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Places the cursor at `position`, clamped to the buffer length.
    pub fn set_cursor(&mut self, position: usize) {
        self.cursor = position.min(self.chars.len());
    }

    /// Clears the buffer.
    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    /// Moves the cursor left.
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor right.
    pub fn move_right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    /// Moves the cursor to the start.
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Moves the cursor to the end.
    pub fn move_end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Inserts a character at the cursor.
    pub fn insert(&mut self, ch: char) {
        self.chars.insert(self.cursor, ch);
        self.cursor += 1;
    }

    /// Deletes the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.chars.remove(self.cursor - 1);
            self.cursor -= 1;
        }
    }

    /// Deletes the character at the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_and_cursor_movement() {
        let mut line = LineEditor::from_text(":event 90643F");
        line.move_end();
        line.insert(' ');
        assert_eq!(line.text(), ":event 90643F ");
        line.move_home();
        assert_eq!(line.cursor(), 0);
        line.move_right();
        line.delete();
        assert_eq!(line.text(), ":vent 90643F ");
        line.backspace();
        assert_eq!(line.text(), "vent 90643F ");
    }
}
